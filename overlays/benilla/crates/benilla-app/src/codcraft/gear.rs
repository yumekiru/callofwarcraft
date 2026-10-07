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

struct WeaponItem {
    code: u32,
    alias: String,
    category: String,
    icon: String,
    name: String,
}

const WEAPON_DISPLAY_BASE: u32 = 1_000_000;

fn weapon_item(entry: u32) -> Option<&'static WeaponItem> {
    if !enabled() { return None; }
    static ITEMS: std::sync::OnceLock<HashMap<u32, WeaponItem>> = std::sync::OnceLock::new();
    ITEMS.get_or_init(|| {
        let root = PathBuf::from(std::env::var_os("CODCRAFT_GEAR_ROOT").unwrap());
        std::fs::read_to_string(root.join("weapon-item-map.tsv")).unwrap_or_default().lines()
            .filter_map(|line| {
                let fields: Vec<_> = line.split('\t').collect();
                if fields.len() != 7 { return None; }
                Some((fields[0].parse().ok()?, WeaponItem {
                    code: fields[1].parse().ok()?, alias: fields[2].to_owned(),
                    category: fields[3].to_owned(), icon: fields[4].to_owned(), name: fields[5].to_owned(),
                }))
            }).collect()
    }).get(&entry)
}

pub(crate) fn authored_weapon_name(base: &str) -> bool {
    if !enabled() { return false; }
    static NAMES: std::sync::OnceLock<std::collections::HashSet<String>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        let root = PathBuf::from(std::env::var_os("CODCRAFT_GEAR_ROOT").unwrap());
        std::fs::read_to_string(root.join("weapon-item-map.tsv")).unwrap_or_default().lines()
            .filter_map(|line| line.split('\t').nth(5).map(str::to_owned)).collect()
    }).contains(base)
}

/// Deliberate fork: per-item firearm art/categories, not shared Warcraft model IDs.
pub(crate) fn decorate_view(entry: u32, view: &mut benilla_ui::script::ItemTemplateView) {
    let Some(item) = weapon_item(entry) else { return; };
    view.name = item.name.clone();
    view.item_type = Some("Weapon".to_owned());
    view.item_sub_type = Some(item.category.clone());
    view.sub_class_display = Some(item.category.clone());
    view.hide_subclass = false;
    view.icon = Some(format!("Interface\\CoDCraftIcons\\{}", item.icon));
    view.allowable_class = -1;
    view.required_skill = 0;
    view.required_skill_rank = 0;
    view.required_skill_name = None;
    view.required_spell = 0;
    view.required_spell_name = None;
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
    if let Some(item) = weapon_item(entry) {
        info.name = item.name.clone();
        // Clone the owned client's original display row at setup, retaining models
        // while giving each item its own icon in every inventory/quest/mail surface.
        info.display_info_id = WEAPON_DISPLAY_BASE + entry;
        info.allowable_class = -1;
        info.required_skill = 0;
        info.required_skill_rank = 0;
        info.required_spell = 0;
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
    if let Ok(map) = std::fs::read_to_string(PathBuf::from(std::env::var_os("CODCRAFT_GEAR_ROOT").unwrap()).join("weapon-item-map.tsv")) {
        for line in map.lines() {
            let fields: Vec<_> = line.split('\t').collect();
            if fields.len() != 7 { continue; }
            let (Ok(entry), Ok(original)) = (fields[0].parse::<u32>(), fields[6].parse::<u32>()) else { continue; };
            if let Some(mut row) = rows.get(&original).cloned() {
                row.icon = Some(format!("Interface\\CoDCraftIcons\\{}", fields[4]));
                rows.insert(WEAPON_DISPLAY_BASE + entry, row);
            }
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
    let mut alias = String::new();
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
                if let Some(item) = weapon_item(entry).filter(|_| info.class == 2) {
                    code = item.code;
                    alias = item.alias.clone();
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
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&stamp.to_le_bytes());
    bytes.extend_from_slice(&code.to_le_bytes());
    bytes.extend_from_slice(&code.to_le_bytes());
    bytes.extend_from_slice(&(alias.len() as u32).to_le_bytes());
    bytes.extend_from_slice(alias.as_bytes());
    let temporary = path.with_extension("gear-pending");
    if std::fs::write(&temporary, bytes).is_ok() && std::fs::rename(temporary, path.with_extension("gear")).is_ok() {
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
