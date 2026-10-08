use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use asset_core::AssetNamespace;

use crate::cue::{CueRequest, CueResolver, ResolvedCue};
use crate::media::{LiveGain, LivePan, RenderMedia};
use crate::pending::PendingTicket;
use crate::render_core::AudioScope;
use crate::spatial::{ListenerSnapshot, SpatialSource};
use crate::start::{SoundClass, StartDecision, StartFailure, StartOutcome, SuppressReason};

const CANCEL_KEYS: usize = 4096;

#[derive(Clone, PartialEq, Eq, Hash)]
struct CancelKey {
    namespace: AssetNamespace,
    alias: String,
    emitter: Option<u32>,
    epoch: u64,
    scope: AudioScope,
}

#[derive(Clone)]
pub(crate) struct CueLease {
    version: u64,
    clock: Arc<AtomicU64>,
}

impl CueLease {
    pub(crate) fn cancelled(&self) -> bool {
        self.clock.load(Ordering::Acquire) != self.version
    }
}

#[derive(Default)]
pub(crate) struct CueCancellation(Mutex<HashMap<CancelKey, Weak<AtomicU64>>>);

impl CueCancellation {
    pub(crate) fn lease(
        &self,
        namespace: AssetNamespace,
        alias: &str,
        emitter: Option<u32>,
        scope: AudioScope,
        epoch: u64,
    ) -> Option<CueLease> {
        let mut keys = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        let key = CancelKey {
            namespace,
            alias: alias.into(),
            emitter,
            scope,
            epoch,
        };
        if let Some(clock) = keys.get(&key).and_then(Weak::upgrade) {
            return Some(CueLease {
                version: clock.load(Ordering::Acquire),
                clock,
            });
        }
        keys.retain(|_, clock| clock.strong_count() != 0);
        if keys.len() == CANCEL_KEYS {
            return None;
        }
        let clock = Arc::new(AtomicU64::new(0));
        keys.insert(key, Arc::downgrade(&clock));
        Some(CueLease { version: 0, clock })
    }

    pub(crate) fn cancel(
        &self,
        namespace: Option<AssetNamespace>,
        alias: Option<&str>,
        emitter: Option<Option<u32>>,
        epoch: u64,
    ) {
        let mut keys = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        keys.retain(|key, clock| {
            let Some(clock) = clock.upgrade() else {
                return false;
            };
            if key.scope == AudioScope::Match
                && key.epoch == epoch
                && namespace.is_none_or(|namespace| namespace == key.namespace)
                && alias.is_none_or(|alias| alias == key.alias)
                && emitter.is_none_or(|emitter| emitter == key.emitter)
            {
                clock.fetch_add(1, Ordering::AcqRel);
            }
            true
        });
    }
}

#[derive(Clone)]
pub(crate) struct CueMix {
    pub epoch: u64,
    pub gain: LiveGain,
    pub channels: [LiveGain; 64],
}

pub(crate) struct CueTrigger {
    pub event: Option<crate::AudioEvent>,
    pub bank: Arc<asset_audio::SoundCatalog>,
    pub namespace: AssetNamespace,
    pub alias: String,
    pub bound: Option<usize>,
    pub origin_inches: Option<[f32; 3]>,
    pub emitter: Option<u32>,
    pub class: SoundClass,
    pub epoch: u64,
    pub pitch_scale: f32,
    pub fallbacks: Vec<String>,
}

pub(crate) struct CueIntent {
    pub event: Option<crate::AudioEvent>,
    pub origin_inches: Option<[f32; 3]>,
    pub emitter: Option<u32>,
    pub class: SoundClass,
    pub deadline: Instant,
    pub depth: u8,
    pub fallbacks: Vec<String>,
    pub mix: Option<CueMix>,
    pub lease: CueLease,
    pub ticket: PendingTicket,
}

pub(crate) struct CueWork {
    pub request: CueRequest,
    pub resolved: Option<ResolvedCue>,
    secondary: Option<(String, StartOutcome)>,
    pub source: Option<(crate::sources::SourceKey, u64)>,
}

pub(crate) struct CueStart {
    pub looping: bool,
    pub media: RenderMedia,
    pub spatial: Option<SpatialSource>,
    pub admission: crate::admission::AdmissionPolicy,
    pub gain: f32,
    pub rate: f32,
    pub lease: CueLease,
}

pub(crate) enum CueStep {
    Waiting,
    Finished,
    Start(CueStart),
}

impl CueWork {
    pub(crate) fn new(request: CueRequest) -> Self {
        Self {
            request,
            resolved: None,
            secondary: None,
            source: None,
        }
    }

    pub(crate) fn complete(&self, outcome: StartOutcome) {
        self.request.state.resolved(Err(match &outcome {
            StartOutcome::Failed(StartFailure::CueRefused(reason)) => *reason,
            StartOutcome::Failed(StartFailure::MissingAlias) => crate::CueFailure::MissingAlias,
            _ => crate::CueFailure::NoMedia,
        }));
        self.request.state.complete(StartDecision {
            event: self.request.execution.event,
            namespace: self.request.namespace,
            alias: self.request.alias.clone(),
            variant: self.resolved.as_ref().map(|cue| cue.variant),
            outcome,
            secondary: self.secondary.clone(),
            detail: None,
        });
    }

    pub(crate) fn step(
        &mut self,
        resolver: &mut CueResolver,
        listener: Option<ListenerSnapshot>,
        children: &mut Vec<CueWork>,
    ) -> CueStep {
        let intent = &self.request.execution;
        if intent.lease.cancelled() || self.request.state.release.requested() {
            self.complete(StartOutcome::Failed(StartFailure::CueRefused(
                crate::CueFailure::Cancelled,
            )));
            return CueStep::Finished;
        }
        if self.source.is_none() && Instant::now() >= intent.deadline {
            self.complete(StartOutcome::Failed(StartFailure::Expired));
            return CueStep::Finished;
        }
        if self.resolved.is_none() {
            match resolver.resolve(&self.request) {
                Ok(cue) => {
                    self.request.state.resolved(Ok(cue.clone()));
                    if let (Some(media), Some(clip)) = (&cue.media, &cue.clip) {
                        media.request(clip.clone());
                    }
                    self.resolved = Some(cue);
                }
                Err(reason) => {
                    let failure = match reason {
                        crate::CueFailure::MissingAlias => StartFailure::MissingAlias,
                        crate::CueFailure::NoMedia => StartFailure::NoPcm,
                        reason => StartFailure::CueRefused(reason),
                    };
                    return self.fail(failure);
                }
            }
        }
        if self.request.namespace == AssetNamespace::T5 {
            self.secondary(children);
        }
        let cue = self.resolved.as_ref().expect("resolved cue");
        let Some(clip) = &cue.clip else {
            return self.fail(StartFailure::NoPcm);
        };
        let Some(service) = &cue.media else {
            return self.fail(StartFailure::NoPcm);
        };
        let pcm = match service.ready(clip) {
            None => return CueStep::Waiting,
            Some(Err(error)) => return self.fail(error.into()),
            Some(Ok(pcm)) => pcm,
        };
        let intent = &self.request.execution;
        let mut media = RenderMedia::from_buffer(pcm);
        media.release = Some(self.request.state.release.clone());
        if let Some(mix) = &intent.mix {
            media.gain = Some(mix.gain.clone());
            media.channel_gain = cue
                .policy
                .channel
                .and_then(|channel| mix.channels.get(channel as usize))
                .cloned();
        }
        let mut priority = cue
            .policy
            .priority
            .as_ref()
            .map_or(0.0, |priority| priority.evaluate(None));
        let (spatial, gain) = match (intent.origin_inches, &cue.policy.spatial) {
            (Some(origin), Some(policy)) => {
                let Some(listener) = listener else {
                    return if self.source.is_some() {
                        CueStep::Waiting
                    } else {
                        self.fail(StartFailure::NoListener)
                    };
                };
                let policy = match policy {
                    Ok(policy) => policy,
                    Err(reason) => return self.fail(reason.clone()),
                };
                let source = SpatialSource {
                    origin_inches: origin,
                    dist_min: policy.dist_min,
                    dist_max: policy.dist_max,
                    knots: policy.knots.clone(),
                    near_knots: policy.near_knots.clone(),
                    priority: cue.policy.priority.clone(),
                    base_volume: cue.volume.max(0.0),
                };
                let distance = origin
                    .iter()
                    .zip(listener.origin_inches)
                    .map(|(source, listener)| (source - listener).powi(2))
                    .sum::<f32>()
                    .sqrt();
                let attenuation = crate::attenuation::distance_attenuation(
                    &source.knots,
                    source.near_knots.as_deref(),
                    distance,
                    source.dist_min,
                    source.dist_max,
                );
                if !distance.is_finite() || !attenuation.is_finite() || attenuation < 0.0 {
                    return self.fail(StartFailure::FalloffEval);
                }
                let parameters = source.evaluate(listener);
                if self.source.is_none() && parameters.gains == [0.0; 2] {
                    self.complete(StartOutcome::Suppressed(SuppressReason::Inaudible));
                    return CueStep::Finished;
                }
                priority = parameters.priority;
                let pan = LivePan::unity();
                pan.set(parameters.gains[0], parameters.gains[1]);
                media.pan = Some(pan);
                (Some(source), 1.0)
            }
            _ => (None, cue.volume),
        };
        let start = CueStart {
            looping: self.source.is_some()
                // The bridge owns this emitter until its explicit stop/expiry.
                // Effect-class PlayAlias used to play only one PCM iteration.
                || (self.request.namespace==AssetNamespace::Iw4
                    && self.request.alias=="mp_cobra_helicopter")
                || (matches!(intent.class, SoundClass::Music | SoundClass::Ambience)
                    && cue.policy.looping),
            media,
            spatial,
            admission: cue.policy.admission_for(intent.emitter, priority),
            gain,
            rate: cue.pitch,
            lease: intent.lease.clone(),
        };
        if self.request.namespace != AssetNamespace::T5 {
            self.secondary(children);
        }
        CueStep::Start(start)
    }

    fn fail(&mut self, reason: StartFailure) -> CueStep {
        let outcome = StartOutcome::Failed(reason);
        if outcome.allows_binding_fallback() {
            let intent = &mut self.request.execution;
            if !intent.fallbacks.is_empty() {
                self.request.alias = intent.fallbacks.remove(0);
                self.request.bound = None;
                self.resolved = None;
                self.secondary = None;
                return CueStep::Waiting;
            }
        }
        self.request.state.resolved(Err(match &outcome {
            StartOutcome::Failed(StartFailure::MissingAlias) => crate::CueFailure::MissingAlias,
            _ => crate::CueFailure::NoMedia,
        }));
        self.complete(outcome);
        CueStep::Finished
    }

    fn secondary(&mut self, children: &mut Vec<CueWork>) {
        if self.secondary.is_some() {
            return;
        }
        let Some(alias) = self
            .resolved
            .as_ref()
            .and_then(|cue| cue.layer.as_deref())
            .filter(|alias| !alias.is_empty())
        else {
            return;
        };
        let intent = &self.request.execution;
        if intent.depth >= 10 {
            self.secondary = Some((alias.into(), StartOutcome::Failed(StartFailure::NoPcm)));
            return;
        }
        let state = self.request.state.child(alias);
        let mut child = CueWork::new(CueRequest {
            state,
            bank: self.request.bank.clone(),
            media: self.request.media.clone(),
            namespace: self.request.namespace,
            alias: alias.into(),
            bound: None,
            scope: self.request.scope,
            epoch: self.request.epoch,
            pitch_scale: 1.0,
            execution: CueIntent {
                event: intent.event,
                origin_inches: intent.origin_inches,
                emitter: intent.emitter,
                class: intent.class,
                deadline: intent.deadline,
                depth: intent.depth + 1,
                fallbacks: Vec::new(),
                mix: intent.mix.clone(),
                lease: intent.lease.clone(),
                ticket: intent.ticket.clone(),
            },
        });
        child.source = self.source;
        children.push(child);
        self.secondary = Some((alias.into(), StartOutcome::Pending));
    }
}
