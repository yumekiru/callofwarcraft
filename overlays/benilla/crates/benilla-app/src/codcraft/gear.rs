//! Equipment presentation changes only: retail models, stats, quality and suffix rolls stay intact.
use super::*;
use benilla_protocol::ItemInfo;

pub(super) const GUNS: [(&str, &str); 9] = [
    ("AK-47", "ak47"),
    ("ACR", "acr"),
    ("F2000", "f2000"),
    ("FAL", "fal"),
    ("M16A4", "m16"),
    ("RPG-7", "rpg"),
    ("SCAR-H", "scar"),
    ("UMP45", "ump"),
    ("USP .45", "usp"),
];

pub(crate) fn enabled() -> bool {
    passthrough_enabled() && std::env::var_os("CODCRAFT_GEAR_ROOT").is_some_and(|p| !p.is_empty())
}
pub(super) fn gun_code(display: u32) -> u32 {
    display % GUNS.len() as u32 + 1
}

fn armour(inventory: u32) -> Option<(&'static str, &'static str)> {
    match inventory {
        1 => Some(("Combat Helmet", "helmet")),
        2 => Some(("Operator Pendant", "necklace")),
        3 => Some(("Shoulder Rig", "shoulder")),
        4 | 5 | 20 => Some(("Tactical Vest", "torso")),
        6 => Some(("Utility Belt", "belt")),
        7 => Some(("Combat Trousers", "pants")),
        8 => Some(("Patrol Boots", "shoes")),
        9 => Some(("Wrist Guards", "wrist")),
        10 => Some(("Combat Gloves", "gloves")),
        16 => Some(("Field Cape", "back")),
        _ => None,
    }
}

pub(crate) fn rename_template(entry: u32, info: &mut ItemInfo) {
    if !enabled() || info.inventory_type == 0 || !matches!(info.class, 2 | 4) {
        return;
    }
    let label = if info.class == 2 {
        if info.subclass == 16 || info.inventory_type == 25 {
            "Combat Knife"
        } else {
            GUNS[(gun_code(info.display_info_id) - 1) as usize].0
        }
    } else {
        armour(info.inventory_type)
            .map(|a| a.0)
            .unwrap_or(match info.inventory_type {
                3 => "Shoulder Rig",
                9 => "Wrist Guards",
                10 => "Combat Gloves",
                11 => "Signet",
                12 => "Field Device",
                14 => "Ballistic Shield",
                16 => "Field Cape",
                19 => "Unit Tabard",
                _ => "Operator Equipment",
            })
    };
    static NAMES: std::sync::OnceLock<HashMap<u32, String>> = std::sync::OnceLock::new();
    let names = NAMES.get_or_init(|| {
        let path = PathBuf::from(std::env::var_os("CODCRAFT_GEAR_ROOT").unwrap())
            .join("gear-name-map.tsv");
        std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|line| {
            let (entry, name) = line.split_once('\t')?;
            Some((entry.parse().ok()?, name.to_owned()))
        }).collect()
    });
    let Some(name) = names.get(&entry) else { return; };
    let suffix = info
        .name
        .find(" of ")
        .map(|at| info.name[at..].to_owned())
        .unwrap_or_default();
    info.name = format!("{}{suffix}", name.replace("{weapon}", label));
}

#[derive(Resource, Default)]
pub(super) struct GearState {
    pub(super) code: u32,
}

fn configure_icons(
    mut displays: Option<ResMut<crate::entities::ItemDisplays>>,
    mut done: Local<bool>,
) {
    if *done || !enabled() {
        return;
    }
    let Some(ref mut displays) = displays else {
        return;
    };
    let Some(root) = std::env::var_os("CODCRAFT_GEAR_ROOT") else {
        return;
    };
    let Ok(map) = std::fs::read_to_string(PathBuf::from(root).join("gear-display-map.tsv")) else {
        return;
    };
    let mut rows: HashMap<_, _> = displays
        .catalog
        .iter()
        .map(|(id, row)| (id, row.clone()))
        .collect();
    let mut changed = 0;
    for line in map.lines() {
        let columns: Vec<_> = line
            .split('\t')
            .filter_map(|x| x.parse::<u32>().ok())
            .collect();
        if columns.len() != 4 {
            continue;
        }
        let icon = if columns[1] == 2 {
            if columns[3] == 16 || columns[2] == 25 {
                Some("knife")
            } else {
                Some(GUNS[(gun_code(columns[0]) - 1) as usize].1)
            }
        } else {
            armour(columns[2]).map(|a| a.1)
        };
        if let (Some(icon), Some(row)) = (icon, rows.get_mut(&columns[0])) {
            row.icon = Some(format!("Interface\\CoDCraftIcons\\{icon}"));
            changed += 1;
        }
    }
    displays.catalog = benilla_formats::ItemDisplayCatalog::from_displays(rows);
    *done = true;
    info!("CoDCraft: replaced {changed} equipment icon mappings; unsupported armour slots retain original art");
}

pub(super) fn sync_equipment(
    time: Res<Time>,
    live: Res<benilla_world::schedule::WorldLive>,
    self_guid: Res<crate::net::SelfGuid>,
    objects: crate::net::Objects,
    items: Res<crate::items::Items>,
    net: Res<crate::net::NetCommands>,
    mut selected: ResMut<GearState>,
    mut last: Local<(u32, f32, u64)>,
) {
    if !enabled() {
        return;
    }
    let mut code = 0;
    if live.0 {
        if let Some(player) = self_guid.0.and_then(|g| objects.object(g)) {
            for slot in [15, 17, 16] {
                let Some(guid) = player.player_inv_slot(slot).filter(|g| *g != 0) else {
                    continue;
                };
                let Some(entry) = objects.object(guid).and_then(|f| f.object_entry()) else {
                    break;
                };
                let Some(info) = items.template(entry, guid, &net) else {
                    break;
                };
                if info.class == 2 && info.subclass != 16 && info.inventory_type != 25 {
                    code = gun_code(info.display_info_id);
                    break;
                }
            }
        }
    }
    selected.code = code;
    let now = time.elapsed_secs();
    if code == last.0 && now < last.1 {
        return;
    }
    let Some(path) = std::env::var_os("CODCRAFT_INPUT").map(PathBuf::from) else {
        return;
    };
    last.2 = last.2.wrapping_add(1);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"CCGE");
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&stamp.to_le_bytes());
    bytes.extend_from_slice(&code.to_le_bytes());
    bytes.extend_from_slice(&code.to_le_bytes());
    if std::fs::write(path.with_extension("gear"), bytes).is_ok() {
        if code != last.0 {
            info!("CoDCraft: equipped guest gun code {code}");
        }
        last.0 = code;
        last.1 = now + 0.25;
    }
}

pub(super) fn plugin(app: &mut App) {
    app.init_resource::<GearState>()
        .add_systems(Update, configure_icons)
        .add_systems(
            Update,
            sync_equipment
                .before(publish_guest_input)
                .before(present_viewmodel),
        );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_supplied_armour_slots_receive_new_art() {
        assert_eq!(armour(1).unwrap().1, "helmet");
        assert_eq!(armour(20).unwrap().1, "torso");
        for slot in [11, 12, 14, 19] {
            assert!(armour(slot).is_none());
        }
        for id in 0..40000 {
            assert!((1..=9).contains(&gun_code(id)));
        }
    }
}
