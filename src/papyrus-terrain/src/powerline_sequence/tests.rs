use super::*;

fn policy(spacing: usize) -> Policy {
    Policy {
        engine: VERSION.into(),
        regular_spacing: spacing,
    }
}

#[test]
fn regular_and_platform_picks_produce_the_same_source_pattern() {
    let rules = [Some(Rule::Regular), Some(Rule::Platform)];
    let pool = Pool::new(&rules, &policy(2)).unwrap();
    for chosen in [0, 1] {
        let mut history = History::default();
        let mut actual = Vec::new();
        for index in 0..9 {
            let selected = pool
                .replacement(rules[chosen], history, 9, index)
                .unwrap()
                .unwrap_or(chosen);
            actual.push(selected);
            history.accept(rules[selected]).unwrap();
        }
        assert_eq!(actual, [1, 0, 0, 1, 0, 0, 1, 0, 1]);
    }
}

#[test]
fn missing_alternatives_plain_fallbacks_and_failed_attempt_history_match_source_rules() {
    let regular_only = Pool::new(&[Some(Rule::Regular)], &policy(2)).unwrap();
    assert_eq!(
        regular_only
            .replacement(Some(Rule::Regular), History::default(), 5, 0)
            .unwrap(),
        None
    );
    let mixed = Pool::new(
        &[None, Some(Rule::Platform), Some(Rule::Regular)],
        &policy(2),
    )
    .unwrap();
    assert_eq!(
        mixed
            .replacement(Some(Rule::Regular), History::default(), 5, 0)
            .unwrap(),
        Some(0)
    );
    let rules = [Some(Rule::Regular), Some(Rule::Platform)];
    let pool = Pool::new(&rules, &policy(2)).unwrap();
    let mut history = History::default();
    // The first fitted platform was rejected. The next slot still starts history.
    assert_eq!(pool.replacement(rules[0], history, 5, 0).unwrap(), Some(1));
    assert_eq!(pool.replacement(rules[0], history, 5, 1).unwrap(), Some(1));
    history.accept(Some(Rule::Platform)).unwrap();
    assert_eq!(pool.replacement(rules[1], history, 5, 2).unwrap(), Some(0));
    assert_eq!(pool.replacement(rules[0], history, 5, 4).unwrap(), Some(1));
    for length in [1, 2] {
        assert_eq!(
            pool.replacement(Some(Rule::Platform), history, length, 0)
                .unwrap(),
            Some(0)
        );
        assert_eq!(
            pool.replacement(Some(Rule::Regular), history, length, 0)
                .unwrap(),
            None
        );
    }
}

#[test]
fn constant_time_history_matches_independent_backward_scans_including_plain_prefabs() {
    let rosters = [
        vec![Some(Rule::Regular), Some(Rule::Platform)],
        vec![None, Some(Rule::Regular), Some(Rule::Platform)],
        vec![Some(Rule::Platform)],
    ];
    for spacing in [1, 2, 3] {
        for rules in &rosters {
            let pool = Pool::new(rules, &policy(spacing)).unwrap();
            for code in 0..3usize.pow(6) {
                let mut value = code;
                let mut previous = Vec::new();
                let mut history = History::default();
                for _ in 0..6 {
                    let rule = [None, Some(Rule::Platform), Some(Rule::Regular)][value % 3];
                    value /= 3;
                    previous.push(rule);
                    history.accept(rule).unwrap();
                }
                let since = previous
                    .iter()
                    .rev()
                    .take_while(|r| **r != Some(Rule::Platform))
                    .filter(|r| **r == Some(Rule::Regular))
                    .count();
                for length in 1..=8 {
                    for index in 0..length {
                        for selected in [None, Some(Rule::Platform), Some(Rule::Regular)] {
                            let target = match selected {
                                Some(Rule::Regular)
                                    if length >= 3
                                        && (previous.is_empty()
                                            || index == length - 1
                                            || since >= spacing) =>
                                {
                                    Some(Rule::Platform)
                                }
                                Some(Rule::Platform)
                                    if length < 3
                                        || (since < spacing
                                            && since != previous.len()
                                            && index < length - 1) =>
                                {
                                    Some(Rule::Regular)
                                }
                                _ => None,
                            };
                            let expected = target.and_then(|target| {
                                rules.iter().position(|r| r.is_none() || *r == Some(target))
                            });
                            assert_eq!(
                                pool.replacement(selected, history, length, index).unwrap(),
                                expected
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn invalid_policy_slots_and_exhausted_history_are_atomic() {
    assert!(Pool::new(&[], &policy(2)).is_err());
    assert!(Pool::new(&[None], &policy(0)).is_err());
    let mut invalid = policy(2);
    invalid.engine = "future".into();
    assert!(Pool::new(&[None], &invalid).is_err());
    let pool = Pool::new(&[None], &policy(2)).unwrap();
    assert!(pool.replacement(None, History::default(), 0, 0).is_err());
    assert!(pool.replacement(None, History::default(), 3, 3).is_err());
    let mut history = History {
        accepted: 1_000_000,
        regular_since_platform: 2,
    };
    assert!(history.accept(Some(Rule::Platform)).is_err());
    assert_eq!(history.accepted, 1_000_000);
    assert_eq!(history.regular_since_platform, 2);
}
