use super::{Cached, Layers, Source};
use std::{
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

type ResultSource = Result<Option<Source>, String>;

#[derive(Default)]
struct State {
    cached: Option<Cached>,
    active: Vec<Arc<Flight>>,
}

#[derive(Default)]
pub(super) struct Variants {
    state: Mutex<State>,
}

struct Flight {
    key: [u8; 32],
    base: Arc<Layers>,
    state: Mutex<FlightState>,
    done: Condvar,
}

struct FlightState {
    subscribers: usize,
    accepting: bool,
    result: Option<ResultSource>,
}

pub(super) enum Request<'a> {
    Cached(Source),
    Build(Builder<'a>),
    Wait(Waiter),
}

impl Variants {
    pub(super) fn request(&self, key: [u8; 32], base: &Arc<Layers>) -> Request<'_> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = state
            .cached
            .as_ref()
            .filter(|value| value.matches(key, base))
        {
            return Request::Cached(cached.source.clone());
        }
        for flight in &state.active {
            if flight.key == key && (Arc::ptr_eq(&flight.base, base) || flight.base == *base) {
                let mut active = flight.state.lock().unwrap_or_else(|e| e.into_inner());
                if active.accepting {
                    active.subscribers += 1;
                    return Request::Wait(Waiter {
                        flight: flight.clone(),
                    });
                }
            }
        }

        // Retain at most one completed variant, and make room before building a new one.
        state.cached = None;
        let flight = Arc::new(Flight {
            key,
            base: base.clone(),
            state: Mutex::new(FlightState {
                subscribers: 1,
                accepting: true,
                result: None,
            }),
            done: Condvar::new(),
        });
        state.active.push(flight.clone());
        Request::Build(Builder {
            variants: self,
            flight: Some(flight),
        })
    }
}

pub(super) struct Builder<'a> {
    variants: &'a Variants,
    flight: Option<Arc<Flight>>,
}

impl Builder<'_> {
    pub(super) fn should_cancel(&self, initiating_job_cancelled: bool) -> bool {
        if !initiating_job_cancelled {
            return false;
        }
        let flight = self
            .flight
            .as_ref()
            .expect("Terrain build has already finished");
        let mut state = flight.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.subscribers == 1 {
            // A job arriving after cancellation commits must start fresh.
            state.accepting = false;
            true
        } else {
            false
        }
    }

    pub(super) fn finish(mut self, result: &ResultSource) {
        self.publish(result.clone());
    }

    fn publish(&mut self, result: ResultSource) {
        let Some(flight) = self.flight.take() else {
            return;
        };
        let mut variants = self
            .variants
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut state = flight.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.accepting
            && let Ok(Some(source)) = &result
        {
            variants.cached = Some(Cached {
                key: flight.key,
                source: source.clone(),
            });
        }
        variants.active.retain(|entry| !Arc::ptr_eq(entry, &flight));
        state.result = Some(result);
        state.accepting = false;
        flight.done.notify_all();
    }
}

impl Drop for Builder<'_> {
    fn drop(&mut self) {
        self.publish(Err(
            "Shared terrain worker ended before completing generation".into(),
        ));
    }
}

pub(super) struct Waiter {
    flight: Arc<Flight>,
}

impl Waiter {
    pub(super) fn wait(self, cancelled: impl Fn() -> bool) -> ResultSource {
        let mut state = self.flight.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if cancelled() {
                return Ok(None);
            }
            if let Some(result) = &state.result {
                return result.clone();
            }
            // Cancellation belongs to the subscriber, so it need not wait for the producer.
            (state, _) = self
                .flight
                .done
                .wait_timeout(state, Duration::from_millis(25))
                .unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        let mut state = self.flight.state.lock().unwrap_or_else(|e| e.into_inner());
        state.subscribers -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn source() -> Source {
        let base = Arc::new(Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: 3,
            surface_resolution: 2,
            height: vec![18000; 9],
            biome: vec![0; 20],
            splat: vec![0; 32],
            topology: vec![0; 4],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        });
        Source {
            native_base: Some(base.clone()),
            layers: base,
            terrain: None,
            rivers: None,
            palette: None,
            key: crate::Project::default().terrain_key(),
            image: eframe::egui::ColorImage::new([0, 0], Vec::new()),
            heights: Arc::new(vec![18000; 9]),
        }
    }

    #[test]
    fn matching_jobs_share_build_and_the_surviving_job_keeps_it_running() {
        let variants = Variants::default();
        let source = source();
        let base = source.native_base.as_ref().unwrap();
        let key = [3; 32];
        let Request::Build(producer) = variants.request(key, base) else {
            panic!("First job should start terrain");
        };
        let equivalent_base = Arc::new(base.as_ref().clone());
        let Request::Wait(waiter) = variants.request(key, &equivalent_base) else {
            panic!("Matching terrain should join the active build");
        };
        let Request::Wait(cancelled_waiter) = variants.request(key, base) else {
            panic!("Third job should also subscribe");
        };
        assert!(cancelled_waiter.wait(|| true).unwrap().is_none());
        assert!(
            !producer.should_cancel(true),
            "The remaining job still needs terrain"
        );
        std::thread::scope(|scope| {
            let waiter = scope.spawn(move || waiter.wait(|| false));
            producer.finish(&Ok(Some(source.clone())));
            let result = waiter.join().unwrap().unwrap().unwrap();
            assert!(Arc::ptr_eq(&result.layers, &source.layers));
        });
        assert!(matches!(variants.request(key, base), Request::Cached(_)));
    }

    #[test]
    fn distinct_variants_build_independently_and_abandoned_work_can_be_retried() {
        let variants = Variants::default();
        let source = source();
        let base = source.native_base.as_ref().unwrap();
        let key = [1; 32];
        let Request::Build(producer) = variants.request(key, base) else {
            panic!()
        };
        let different = Arc::new(Layers {
            terrain_seed: 3,
            ..base.as_ref().clone()
        });
        assert!(matches!(
            variants.request(key, &different),
            Request::Build(_)
        ));
        assert!(matches!(variants.request([2; 32], base), Request::Build(_)));

        let Request::Wait(waiter) = variants.request(key, base) else {
            panic!()
        };
        let stop = AtomicBool::new(false);
        let entered = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let entered_ref = &entered;
            let stop_ref = &stop;
            let follower = scope.spawn(move || {
                let announced = std::cell::Cell::new(false);
                waiter.wait(|| {
                    if !announced.replace(true) {
                        entered_ref.wait();
                    }
                    stop_ref.load(Ordering::Relaxed)
                })
            });
            entered.wait();
            stop.store(true, Ordering::Relaxed);
            assert!(follower.join().unwrap().unwrap().is_none());
        });
        assert!(producer.should_cancel(true));
        let Request::Build(replacement) = variants.request(key, base) else {
            panic!("A new job must not join a cancelled build");
        };
        producer.finish(&Ok(None));
        replacement.finish(&Ok(Some(source.clone())));
        assert!(matches!(variants.request(key, base), Request::Cached(_)));
    }

    #[test]
    fn worker_failure_wakes_waiters_and_does_not_poison_the_next_build() {
        let variants = Variants::default();
        let source = source();
        let base = source.native_base.as_ref().unwrap();
        let key = [1; 32];
        let Request::Build(producer) = variants.request(key, base) else {
            panic!()
        };
        let Request::Wait(waiter) = variants.request(key, base) else {
            panic!()
        };
        drop(producer);
        assert_eq!(
            waiter.wait(|| false).err().as_deref(),
            Some("Shared terrain worker ended before completing generation")
        );
        assert!(matches!(variants.request(key, base), Request::Build(_)));
    }
}
