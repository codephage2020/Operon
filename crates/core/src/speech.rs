use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpeechStatus {
    Interim,
    Final,
    Revision,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentUpdate {
    pub id: String,
    pub text: String,
    pub status: SpeechStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpeechEvent {
    Interim { id: String, text: String },
    Translate { id: String, text: String },
    Revise { id: String, text: String },
    Overflow,
}

pub struct SpeechCoordinator {
    queue: VecDeque<(String, String)>,
    max_pending: usize,
    overflowing: bool,
}

impl SpeechCoordinator {
    pub fn new(max_pending: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            max_pending: max_pending.max(1),
            overflowing: false,
        }
    }
    pub fn push(&mut self, update: SegmentUpdate) -> Vec<SpeechEvent> {
        match update.status {
            SpeechStatus::Interim => vec![SpeechEvent::Interim {
                id: update.id,
                text: update.text,
            }],
            SpeechStatus::Final => {
                if self.queue.len() >= self.max_pending {
                    self.overflowing = true;
                    return vec![SpeechEvent::Overflow];
                }
                self.queue
                    .push_back((update.id.clone(), update.text.clone()));
                vec![SpeechEvent::Translate {
                    id: update.id,
                    text: update.text,
                }]
            }
            SpeechStatus::Revision => {
                if let Some((_, text)) = self.queue.iter_mut().find(|(id, _)| id == &update.id) {
                    *text = update.text.clone();
                }
                vec![SpeechEvent::Revise {
                    id: update.id,
                    text: update.text,
                }]
            }
        }
    }
    pub fn acknowledge(&mut self, id: &str) {
        if let Some(index) = self.queue.iter().position(|(item, _)| item == id) {
            self.queue.remove(index);
        }
    }
    pub fn finish(&mut self) -> Vec<(String, String)> {
        self.queue.drain(..).collect()
    }
    pub fn pending(&self) -> usize {
        self.queue.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interim_never_enters_translation_queue() {
        let mut coordinator = SpeechCoordinator::new(2);
        assert!(matches!(
            coordinator.push(SegmentUpdate {
                id: "1".into(),
                text: "hel".into(),
                status: SpeechStatus::Interim
            })[0],
            SpeechEvent::Interim { .. }
        ));
        assert_eq!(coordinator.pending(), 0);
        coordinator.push(SegmentUpdate {
            id: "1".into(),
            text: "hello".into(),
            status: SpeechStatus::Final,
        });
        coordinator.push(SegmentUpdate {
            id: "1".into(),
            text: "hello world".into(),
            status: SpeechStatus::Revision,
        });
        assert_eq!(
            coordinator.finish(),
            vec![("1".into(), "hello world".into())]
        );
    }
    #[test]
    fn bounded_queue_reports_overflow() {
        let mut coordinator = SpeechCoordinator::new(1);
        coordinator.push(SegmentUpdate {
            id: "1".into(),
            text: "one".into(),
            status: SpeechStatus::Final,
        });
        assert_eq!(
            coordinator.push(SegmentUpdate {
                id: "2".into(),
                text: "two".into(),
                status: SpeechStatus::Final
            }),
            vec![SpeechEvent::Overflow]
        );
    }
}
