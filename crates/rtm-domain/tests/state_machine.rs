// SPDX-License-Identifier: AGPL-3.0-or-later
//! Property tests: no sequence of events can drive an item through an illegal transition.

use proptest::prelude::*;
use rtm_domain::{FailureCode, ItemState, Stage};

#[derive(Debug, Clone)]
enum Event {
    Complete(Stage),
    Fail(Stage, FailureCode),
}

fn stage() -> impl Strategy<Value = Stage> {
    proptest::sample::select(Stage::ALL.to_vec())
}

fn code() -> impl Strategy<Value = FailureCode> {
    proptest::sample::select(FailureCode::ALL.to_vec())
}

fn event() -> impl Strategy<Value = Event> {
    prop_oneof![
        stage().prop_map(Event::Complete),
        (stage(), code()).prop_map(|(s, c)| Event::Fail(s, c)),
    ]
}

fn index(stage: Stage) -> usize {
    Stage::ALL.iter().position(|s| *s == stage).unwrap()
}

proptest! {
    #[test]
    fn transitions_only_ever_move_forward(events in proptest::collection::vec(event(), 0..40)) {
        let mut state = ItemState::submitted();
        for ev in events {
            let before = state;
            let result = match ev {
                Event::Complete(s) => state.complete(s),
                Event::Fail(s, c) => state.fail(s, c),
            };
            match (before, result) {
                // Terminal states never change.
                (b, Ok(_)) if b.is_terminal() => prop_assert!(false, "{b:?} changed"),
                (ItemState::Pending { next }, Ok(after)) => {
                    let (ev_stage, failed) = match ev {
                        Event::Complete(s) => (s, None),
                        Event::Fail(s, c) => (s, Some(c)),
                    };
                    // Only the stage being waited for may act.
                    prop_assert_eq!(ev_stage, next);
                    match (failed, after) {
                        (Some(c), ItemState::Failed { stage, code }) => {
                            prop_assert_eq!(stage, next);
                            prop_assert_eq!(code, c);
                        }
                        (None, ItemState::Pending { next: n }) => {
                            prop_assert_eq!(index(n), index(next) + 1);
                        }
                        (None, ItemState::Published) => prop_assert_eq!(next, Stage::Publish),
                        (f, a) => prop_assert!(false, "{next:?} + {f:?} gave {a:?}"),
                    }
                    state = after;
                }
                (_, Err(e)) => {
                    // A rejected event leaves the state as it was.
                    prop_assert_eq!(e.from, before);
                }
                (b, Ok(a)) => prop_assert!(false, "unexpected {b:?} -> {a:?}"),
            }
        }
    }
}
