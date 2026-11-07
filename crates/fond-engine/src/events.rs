//! Шина подій рушія: журнал і сповіщення для UI (broadcast).

use fond_api::events::EngineEventDto;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<EngineEventDto>,
}

impl EventBus {
    /// `cap` — розмір буфера; повільний підписник пропускає найстаріші події.
    pub fn new(cap: usize) -> Self {
        let (tx, _) = broadcast::channel(cap.max(1));
        Self { tx }
    }

    /// Без підписників подія просто відкидається.
    pub fn publish(&self, e: EngineEventDto) {
        let _ = self.tx.send(e);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EngineEventDto> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_without_subscribers_is_ignored() {
        let bus = EventBus::new(4);
        bus.publish(EngineEventDto::StatusChanged);
    }

    #[test]
    fn subscribers_receive_published_events() {
        let bus = EventBus::new(4);
        let mut a = bus.subscribe();
        let mut b = bus.clone().subscribe();
        bus.publish(EngineEventDto::AutoPaused { reason: "x".into() });
        let want = EngineEventDto::AutoPaused { reason: "x".into() };
        assert_eq!(a.try_recv().unwrap(), want);
        assert_eq!(b.try_recv().unwrap(), want);
        assert!(a.try_recv().is_err());
    }

    #[test]
    fn zero_capacity_does_not_panic() {
        let bus = EventBus::new(0);
        let mut rx = bus.subscribe();
        bus.publish(EngineEventDto::StatusChanged);
        assert_eq!(rx.try_recv().unwrap(), EngineEventDto::StatusChanged);
    }
}
