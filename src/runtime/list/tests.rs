use super::{SCRATCH_LIST_LIMIT, ScratchList, ScratchValue, scratch_equals};

#[test]
fn finite_generated_numbers_match_generic_appends_and_keep_list_flags() {
    let values = [11.0, 14.0, 20.0, 12.0];
    let mut finite = ScratchList::new(Vec::new());
    let mut generic = ScratchList::new(Vec::new());
    let mut finite_index = 0;
    let mut generic_index = 0;

    assert!(finite.add_generated_finite_numbers(values.len(), || {
        let value = values[finite_index];
        finite_index += 1;
        value
    }));
    assert!(generic.add_generated_numbers(values.len(), || {
        let value = values[generic_index];
        generic_index += 1;
        value
    }));
    assert_eq!(finite.items(), generic.items());
    assert!(finite.all_numbers && finite.all_finite_numbers);

    let mut non_finite = ScratchList::new(vec![ScratchValue::Number(f64::INFINITY)]);
    assert!(non_finite.add_generated_finite_numbers(1, || 11.0));
    assert!(!non_finite.all_finite_numbers);
    assert_eq!(non_finite.get(2.0), &ScratchValue::Number(11.0));

    let mut generic_non_finite = ScratchList::new(Vec::new());
    assert!(generic_non_finite.add_generated_numbers(2, || f64::INFINITY));
    assert!(!generic_non_finite.all_finite_numbers);
}

#[test]
fn generated_number_appender_keeps_old_renderer_snapshots_and_updates_new_ones() {
    let mut list = ScratchList::new((0..1024).map(|n| ScratchValue::Number(n as f64)).collect());
    list.with_numeric_replacements(|_| {});
    let old_snapshot = list.shared_renderer_numeric_items().unwrap();
    let mut next = 1024.0;

    let changed = list.with_generated_finite_number_appender(3, |append| {
        assert!(append.add_finite_numbers(1, || {
            let value = next;
            next += 1.0;
            value
        }));
        append.add_finite_numbers(8, || {
            let value = next;
            next += 1.0;
            value
        })
    });

    assert!(changed);
    assert_eq!(list.length(), 1027);
    assert_eq!(list.get(1025.0), &ScratchValue::Number(1024.0));
    assert_eq!(list.get(1027.0), &ScratchValue::Number(1026.0));
    assert_eq!(old_snapshot.len(), 1024);
    assert_eq!(old_snapshot.get(1023), Some(1023.0));

    let new_snapshot = list.shared_renderer_numeric_items().unwrap();
    assert_eq!(new_snapshot.len(), 1027);
    assert_eq!(new_snapshot.get(1024), Some(1024.0));
    assert_eq!(new_snapshot.get(1026), Some(1026.0));
}

#[test]
fn generated_number_appender_preserves_mixed_lists_and_the_scratch_limit() {
    let mut mixed = ScratchList::new(vec![ScratchValue::Text("head".into())]);
    let mut generated = 0;
    assert!(mixed.with_generated_finite_number_appender(3, |append| {
        append.add_finite_numbers(2, || {
            generated += 1;
            generated as f64
        })
    }));
    assert_eq!(generated, 2);
    assert_eq!(mixed.items(), &["head".into(), 1.0.into(), 2.0.into()]);
    assert!(!mixed.all_numbers);
    assert!(!mixed.all_finite_numbers);

    let mut full = ScratchList::new(vec![ScratchValue::Number(0.0); SCRATCH_LIST_LIMIT - 2]);
    let mut appended = 0;
    assert!(full.with_generated_finite_number_appender(5, |append| {
        append.add_finite_numbers(5, || {
            appended += 1;
            appended as f64
        })
    }));
    assert_eq!(appended, 2);
    assert_eq!(full.length(), SCRATCH_LIST_LIMIT);

    let mut already_full = ScratchList::new(vec![ScratchValue::Number(0.0); SCRATCH_LIST_LIMIT]);
    let mut called = false;
    assert!(
        !already_full.with_generated_finite_number_appender(1, |append| {
            append.add_finite_numbers(1, || {
                called = true;
                1.0
            })
        })
    );
    assert!(!called);
}

#[test]
fn cached_selector_reads_preserve_values_and_invalidate_on_mutation() {
    let mut list = ScratchList::new(vec!["12".into(), "last".into(), "NaN".into(), 4.0.into()]);
    assert_eq!(list.get_cached_number(1.0), 12.0);
    assert_eq!(list.get_cached_number(2.0), 0.0);
    assert_eq!(list.get_cached_number(3.0), 0.0);
    assert_eq!(
        list.get_with_cached_selector(2.0).0,
        &ScratchValue::Text("last".into())
    );
    let snapshot = list.clone();
    assert!(list.replace(1.0, "35".into()));
    assert_eq!(list.get_cached_number(1.0), 35.0);
    assert_eq!(snapshot.get_cached_number(1.0), 12.0);
    assert!(list.delete(2.0));
    assert_eq!(list.get_cached_number(2.0), 0.0);
    list.replace_all(vec![2.0.into(), 3.0.into()]);
    assert_eq!(list.get_cached_number(1.0), 2.0);
    list.with_numeric_replacements(|items| {
        items.replace_number_and_equals(1.0, 9.0, 9.0);
    });
    assert_eq!(list.get_cached_number(1.0), 9.0);
}

#[test]
fn numeric_replacement_and_equality_keep_scratch_index_and_number_rules() {
    let mut list = ScratchList::new(vec![
        ScratchValue::Number(0.0),
        ScratchValue::Number(0.0),
        ScratchValue::Number(0.0),
    ]);

    assert!(list.replace_number(2.9, 17.0));
    assert_eq!(list.length(), 3);
    assert_eq!(list.get(2.0), &ScratchValue::Number(17.0));
    assert_eq!(list.equals_number(2.0, 17.0), Some(true));
    assert_eq!(list.equals_number(2.0, 18.0), Some(false));
    assert!(!list.replace_number(2.1, 17.0));
    assert!(!list.replace_number(0.9, 12.0));
    assert!(!list.replace_number(f64::INFINITY, 12.0));
    assert_eq!(list.length(), 3);

    let nan_list = ScratchList::new(vec![ScratchValue::Number(f64::NAN)]);
    assert_eq!(nan_list.equals_number(1.0, 42.0), Some(true));

    let mut mixed = ScratchList::new(vec![ScratchValue::Number(1.0), "two".into()]);
    assert_eq!(mixed.equals_number(1.0, 1.0), None);
    assert!(mixed.replace_number(2.0, 3.0));
    assert_eq!(mixed.get(2.0), &ScratchValue::Number(3.0));
}

#[test]
fn numeric_replacement_can_read_back_through_the_same_validated_index() {
    let mut list = ScratchList::new(vec![ScratchValue::Number(0.0), "old".into()]);
    assert_eq!(
        list.replace_number_and_equals(2.0, 17.0, 17.0),
        (true, true)
    );
    assert_eq!(
        list.replace_number_and_equals(2.0, 17.0, 17.0),
        (false, true)
    );
    assert_eq!(
        list.replace_number_and_equals(2.9, 18.0, 18.0),
        (true, true)
    );
    assert_eq!(
        list.replace_number_and_equals(0.0, 17.0, 17.0),
        (false, false)
    );
    assert_eq!(
        list.replace_number_and_equals(-1.0, 19.0, 19.0),
        (false, false)
    );
    assert_eq!(
        list.replace_number_and_equals(3.0, 20.0, 20.0),
        (false, false)
    );
    assert_eq!(
        list.replace_number_and_equals(f64::NAN, 21.0, 21.0),
        (false, false)
    );
    assert_eq!(
        list.replace_number_and_equals(f64::INFINITY, 22.0, 22.0),
        (false, false)
    );
    assert_eq!(list.get(2.0), &ScratchValue::Number(18.0));
    assert_eq!(
        list.replace_number_and_equals(1.0, f64::NAN, f64::NAN),
        (true, true)
    );
}

#[test]
fn numeric_batch_matches_general_replacement_for_mixed_and_invalid_items() {
    let original = ScratchList::new(vec!["text".into(), 2.0.into()]);
    let mut general = original.clone();
    let mut batched = original;
    let cases = [
        (1.9, 7.0, 7.0),
        (2.0, 2.0, 2.0),
        (0.0, 3.0, 3.0),
        (-1.0, 3.0, 3.0),
        (3.0, 3.0, 3.0),
        (f64::NAN, 3.0, 3.0),
        (f64::INFINITY, 3.0, 3.0),
        (1.0, f64::NAN, f64::NAN),
    ];
    let expected: Vec<_> = cases
        .iter()
        .map(|&(index, item, expected)| {
            let changed = general.replace(index, ScratchValue::Number(item));
            (changed, scratch_equals(general.get(index), expected))
        })
        .collect();
    let observed = batched.with_numeric_replacements(|batch| {
        cases
            .iter()
            .map(|&(index, item, expected)| batch.replace_number_and_equals(index, item, expected))
            .collect::<Vec<_>>()
    });
    assert_eq!(observed, expected);
    assert_eq!(batched.length(), general.length());
    assert!(matches!(batched.get(1.0), ScratchValue::Number(n) if n.is_nan()));
    assert_eq!(batched.get(2.0), general.get(2.0));
}

#[test]
fn compact_numeric_batch_matches_general_path_for_edge_cases() {
    let mut ordinary = ScratchList::new(vec![0.0.into(), 2.0.into()]);
    let mut compact = ordinary.clone();
    let cases = [
        (1.9, 7.0, 7.0),
        (2.0, 2.0, 2.0),
        (0.0, 3.0, 3.0),
        (-1.0, 3.0, 3.0),
        (3.0, 3.0, 3.0),
        (f64::NAN, 3.0, 3.0),
        (f64::INFINITY, 3.0, 3.0),
        (f64::NEG_INFINITY, 3.0, 3.0),
        (1.0, f64::NAN, f64::NAN),
    ];
    let expected: Vec<_> = cases
        .iter()
        .map(|&(index, item, expected)| {
            let changed = ordinary.replace(index, ScratchValue::Number(item));
            (changed, scratch_equals(ordinary.get(index), expected))
        })
        .collect();
    let observed = compact.with_numeric_replacements(|batch| {
        cases
            .iter()
            .map(|&(index, item, expected)| batch.replace_number_and_equals(index, item, expected))
            .collect::<Vec<_>>()
    });
    assert!(compact.numeric_items.is_some());
    assert_eq!(observed, expected);
    assert!(matches!(compact.get(1.0), ScratchValue::Number(n) if n.is_nan()));
    assert_eq!(compact.get(2.0), ordinary.get(2.0));
}

#[test]
fn compact_batches_keep_snapshots_clones_and_general_edits_coherent() {
    let mut list = ScratchList::new(vec![1.0.into(), 2.0.into(), 3.0.into()]);
    let original = list.shared_items();
    list.with_numeric_replacements(|batch| {
        assert_eq!(batch.replace_number_and_equals(2.0, 8.0, 8.0), (true, true));
    });
    assert!(list.numeric_items.is_some());
    assert_eq!(list.length(), 3);
    let first_snapshot = list.shared_items();
    let clone = list.clone();
    assert!(list.shares_storage_with(&clone));
    list.with_numeric_replacements(|batch| {
        assert_eq!(batch.replace_number_and_equals(1.9, 7.0, 7.0), (true, true));
        assert_eq!(
            batch.replace_number_and_equals(f64::INFINITY, 9.0, 9.0),
            (false, false)
        );
    });
    assert!(!list.shares_storage_with(&clone));
    assert_eq!(original[1], ScratchValue::Number(2.0));
    assert_eq!(first_snapshot[0], ScratchValue::Number(1.0));
    assert_eq!(clone.get(1.0), &ScratchValue::Number(1.0));
    assert_eq!(list.get(1.0), &ScratchValue::Number(7.0));
    assert_eq!(list.get(2.0), &ScratchValue::Number(8.0));
    assert!(list.replace(2.0, "edited".into()));
    assert!(list.numeric_items.is_none());
    assert_eq!(list.get(2.0), &ScratchValue::Text("edited".into()));
    assert_eq!(first_snapshot[1], ScratchValue::Number(8.0));
    assert!(list.insert(4.0.into(), 3.0));
    assert!(list.delete(1.0));
    assert_eq!(list.length(), 3);
    assert_eq!(list.get(1.0), &ScratchValue::Text("edited".into()));
}

#[test]
fn numeric_replacement_keeps_large_list_length_and_boundary_values() {
    // exercise replacement against the fixture's full list length.
    let length = 172_800;
    let mut list = ScratchList::new(vec![ScratchValue::Number(0.0); length]);

    assert_eq!(
        list.replace_number_and_equals(1.0, 11.0, 11.0),
        (true, true)
    );
    assert_eq!(
        list.replace_number_and_equals((length / 2) as f64, 22.0, 22.0),
        (true, true)
    );
    assert_eq!(
        list.replace_number_and_equals(length as f64, 33.0, 33.0),
        (true, true)
    );
    assert_eq!(list.length(), length);
    assert_eq!(list.get(1.0), &ScratchValue::Number(11.0));
    assert_eq!(list.get((length / 2) as f64), &ScratchValue::Number(22.0));
    assert_eq!(list.get(length as f64), &ScratchValue::Number(33.0));
}

#[test]
fn preserves_values_and_mutates_with_scratch_indices() {
    let mut list = ScratchList::new(vec![
        "a".into(),
        2.0_f64.into(),
        true.into(),
        "2".into(),
        "A".into(),
    ]);
    assert_eq!(list.get(2.0_f64), &ScratchValue::Number(2.0));
    assert_eq!(list.get(3.0_f64), &ScratchValue::Boolean(true));
    list.add("tail".into());
    assert_eq!(list.get(1.9_f64), &ScratchValue::Text("a".into()));
    assert_eq!(list.get("last"), &ScratchValue::Text("tail".into()));
    list.replace("2.9", "middle".into());
    assert_eq!(list.get(2.0_f64), &ScratchValue::Text("middle".into()));
    list.insert("front".into(), "1.9");
    assert_eq!(list.get(1.0_f64), &ScratchValue::Text("front".into()));
    list.delete(-1.0_f64);
    list.delete("0");
    assert_eq!(list.length(), 7);
    list.delete("last");
    assert_eq!(list.length(), 6);
    list.delete("all");
    assert!(list.items().is_empty());
}

#[test]
fn compact_initializer_preserves_supported_scratch_value_types() {
    let list = ScratchList::from_json(r#"["text", 4.5, true, null, {"x": 1}]"#);
    assert_eq!(
        list.items(),
        &[
            ScratchValue::Text("text".into()),
            ScratchValue::Number(4.5),
            ScratchValue::Boolean(true),
            ScratchValue::Number(0.0),
            ScratchValue::Number(0.0),
        ]
    );
}

#[test]
fn shared_list_snapshots_use_copy_on_write_and_keep_clones_independent() {
    let original = ScratchList::new(vec!["one".into(), "two".into()]);
    let snapshot = original.shared_items();
    let mut clone = original.clone();
    assert!(original.shares_storage_with(&clone));

    clone.replace(1.0_f64, "clone".into());
    assert!(!original.shares_storage_with(&clone));
    assert_eq!(original.get(1.0_f64), &ScratchValue::Text("one".into()));
    assert_eq!(clone.get(1.0_f64), &ScratchValue::Text("clone".into()));
    assert_eq!(snapshot[0], ScratchValue::Text("one".into()));
}

// keep saved rows alive for readers while preparing reusable capacity for a clear-and-refill.
#[test]
fn clearing_a_shared_large_list_keeps_capacity_without_copying_items() {
    let seed = (0..SCRATCH_LIST_LIMIT)
        .map(|index| ScratchValue::Number(index as f64))
        .collect::<Vec<_>>();
    let mut list = ScratchList::new(seed);
    let old_items = list.shared_items();
    let old_capacity = old_items.capacity();

    assert!(list.delete_all());

    assert!(list.items().is_empty());
    assert!(list.items.capacity() >= old_capacity);
    assert_eq!(old_items.len(), SCRATCH_LIST_LIMIT);
    assert_eq!(
        old_items[SCRATCH_LIST_LIMIT - 1],
        ScratchValue::Number(199_999.0)
    );
    assert!(!list.delete_all());
}

// expose actual mutations so generated code can dirty a monitor without a list clone.
#[test]
fn list_mutations_report_only_actual_changes() {
    let mut list = ScratchList::new(vec![1.0_f64.into()]);
    assert!(list.add(2.0_f64.into()));
    assert!(!list.replace(1.0_f64, 1.0_f64.into()));
    assert!(list.replace(1.0_f64, 3.0_f64.into()));
    assert!(!list.delete(0.0_f64));
    assert!(list.insert(4.0_f64.into(), "last"));
    assert!(list.delete("last"));
    assert!(list.delete_all());
    assert!(!list.delete_all());
}

// preserve the item cap while generating batches into the authoritative list storage.
#[test]
fn generated_append_batches_stop_at_the_scratch_item_limit() {
    let mut list = ScratchList::new(vec![ScratchValue::Number(0.0); SCRATCH_LIST_LIMIT - 2]);
    let mut generated = 0;

    assert!(list.add_generated(5, || {
        generated += 1;
        ScratchValue::Number(generated as f64)
    }));

    assert_eq!(generated, 2);
    assert_eq!(list.length(), SCRATCH_LIST_LIMIT);
    assert_eq!(list.get("last"), &ScratchValue::Number(2.0));
    assert!(!list.add_generated(1, || ScratchValue::Number(3.0)));
}

#[test]
fn numeric_batches_preserve_values_and_clear_without_generic_drop_tracking() {
    let mut list = ScratchList::new(Vec::with_capacity(SCRATCH_LIST_LIMIT));
    let mut next = 0.0;

    assert!(list.add_generated_numbers(SCRATCH_LIST_LIMIT, || {
        next += 1.0;
        next
    }));
    assert!(list.all_numbers);
    assert_eq!(list.length(), SCRATCH_LIST_LIMIT);
    assert_eq!(list.get(1.0_f64), &ScratchValue::Number(1.0));
    assert_eq!(list.get(100_000.0_f64), &ScratchValue::Number(100_000.0));
    assert_eq!(list.get("last"), &ScratchValue::Number(200_000.0));
    assert!(list.delete_all());
    assert!(list.items().is_empty());
    assert!(list.all_numbers);
}

#[test]
fn numeric_storage_hint_tracks_mixed_mutations_without_changing_list_equality() {
    let mut list = ScratchList::new(vec!["text".into()]);
    list.replace(1.0_f64, 4.0_f64.into());

    assert!(!list.all_numbers);
    assert_eq!(list, ScratchList::new(vec![4.0_f64.into()]));

    list.delete_all();
    assert!(list.all_numbers);
    assert!(list.add_generated_numbers(1, || 5.0));
    assert!(list.all_numbers);
}

#[test]
fn equality_search_is_case_insensitive_numeric_and_returns_first_match() {
    let list = ScratchList::new(vec![
        "Hello".into(),
        1.0_f64.into(),
        "1".into(),
        "hello".into(),
    ]);
    assert!(list.contains("HELLO"));
    assert_eq!(list.item_number("hello"), 1);
    assert_eq!(list.item_number(1.0_f64), 2);
    assert_eq!(list.item_number("1"), 2);
    assert_eq!(list.item_number("missing"), 0);
}

#[test]
fn numeric_item_number_search_does_not_materialize_compact_items() {
    let mut list = ScratchList::new(vec![
        ScratchValue::Number(3.0),
        ScratchValue::Number(1.0),
        ScratchValue::Number(3.0),
    ]);
    list.with_numeric_replacements(|items| {
        assert_eq!(
            items.replace_number_and_equals(2.0, 1.0, 1.0),
            (false, true)
        );
    });

    assert_eq!(list.item_number(3.0), 1);
    assert_eq!(list.item_number(1.0), 2);
    assert_eq!(list.item_number(2.0), 0);
    assert!(!list.has_materialized_numeric_items());
}

#[test]
fn compact_numeric_item_search_tracks_replacements_and_cloned_lists() {
    let list = ScratchList::new(vec![ScratchValue::Number(0.0); 512]);
    assert_eq!(list.item_number(0.0), 1);

    let mut clone = list.clone();
    assert!(clone.replace_number(1.0, 2.0));
    assert_eq!(clone.item_number(0.0), 2);
    assert_eq!(clone.item_number(2.0), 1);
    assert_eq!(list.item_number(0.0), 1);

    assert!(clone.replace_number(2.0, 3.0));
    assert_eq!(clone.item_number(0.0), 3);
    assert_eq!(clone.item_number(3.0), 2);

    assert!(clone.replace_number(3.0, 4.0));
    assert_eq!(clone.item_number(0.0), 4);
    assert_eq!(clone.item_number(4.0), 3);
}

#[test]
fn owned_numeric_reads_do_not_materialize_compact_storage() {
    let mut list = ScratchList::new(vec![1.0.into(), 2.0.into(), 3.0.into()]);
    assert!(list.replace_number(2.0, 7.0));

    assert_eq!(list.get_value(2.0), ScratchValue::Number(7.0));
    assert!(!list.has_materialized_numeric_items());
}

#[test]
fn compact_finite_numeric_search_handles_vector_lanes_and_tail() {
    let mut list = ScratchList::new((0..17).map(|i| (1000 + i) as f64).map(Into::into).collect());
    assert!(list.replace_number(1.0, -1.0));

    assert_eq!(list.item_number(-1.0), 1);
    for index in [1_usize, 4, 8, 12, 16] {
        assert_eq!(list.item_number((1000 + index) as f64), index + 1);
    }
    assert_eq!(list.item_number(9999.0), 0);
}

#[test]
fn long_numeric_search_preserves_first_duplicate_after_replacement() {
    let mut list = ScratchList::new(vec![ScratchValue::Number(11.0); 4096]);
    assert!(list.replace_number(100.0, 3.0));
    assert_eq!(list.item_number(3.0), 100);

    assert!(list.replace_number(50.0, 3.0));
    assert_eq!(list.item_number(3.0), 50);

    assert!(list.replace_number(50.0, 11.0));
    assert_eq!(list.item_number(3.0), 100);

    let mut clone = list.clone();
    assert!(clone.replace_number(100.0, 4.0));
    assert_eq!(clone.item_number(3.0), 0);
    assert_eq!(clone.item_number(4.0), 100);
    assert_eq!(list.item_number(3.0), 100);
}

#[test]
fn long_numeric_search_finds_late_values_after_replacement() {
    let mut list = ScratchList::new(
        (0..5000)
            .map(|number| ScratchValue::Number(number as f64))
            .collect(),
    );
    assert!(list.replace_number(5000.0, 9000.0));
    assert_eq!(list.item_number(9000.0), 5000);
    assert_eq!(list.item_number(4095.0), 4096);
}

#[test]
fn reporter_contents_match_scratch_join_rules_and_keep_type_distinctions() {
    assert_eq!(
        ScratchList::new(vec!["a".into(), "β".into()]).contents(),
        "aβ"
    );
    assert_eq!(
        ScratchList::new(vec!["a".into(), "".into()]).contents(),
        "a "
    );
    assert_eq!(
        ScratchList::new(vec![1.0_f64.into(), 2.0_f64.into()]).contents(),
        "1 2"
    );
    assert_eq!(
        ScratchList::new(vec!["1".into(), 1.0_f64.into()]).contents(),
        "1 1"
    );
    assert_eq!(ScratchList::default().contents(), "");
    assert_eq!(
        ScratchList::new(vec!["😀".into(), "x".into()]).contents(),
        "😀 x"
    );
}

#[test]
fn full_list_append_is_rejected_but_valid_insert_drops_the_tail() {
    let mut list = ScratchList::new(vec![ScratchValue::Number(0.0); SCRATCH_LIST_LIMIT]);
    assert!(!list.add("overflow".into()));
    list.insert("front".into(), 1.0_f64);
    assert_eq!(list.length(), SCRATCH_LIST_LIMIT);
    assert_eq!(list.get(1.0_f64), &ScratchValue::Text("front".into()));
    list.insert("tail".into(), SCRATCH_LIST_LIMIT as f64);
    assert_eq!(list.length(), SCRATCH_LIST_LIMIT);
    assert_eq!(list.get("last"), &ScratchValue::Text("tail".into()));
    list.insert("overflow".into(), SCRATCH_LIST_LIMIT as f64 + 1.0);
    assert_eq!(list.get("last"), &ScratchValue::Text("tail".into()));
}

#[test]
fn invalid_and_special_indices_follow_scratch_rules() {
    let mut list = ScratchList::new(vec!["first".into(), "middle".into(), "last".into()]);
    assert_eq!(list.get(0.0_f64), &ScratchValue::Text(String::new()));
    assert_eq!(list.get(-1.0_f64), &ScratchValue::Text(String::new()));
    assert_eq!(list.get(4.0_f64), &ScratchValue::Text(String::new()));
    assert_eq!(list.get("2.9"), &ScratchValue::Text("middle".into()));
    assert_eq!(list.get("last"), &ScratchValue::Text("last".into()));
    assert!(list.items().contains(list.get("random")));
    assert!(list.items().contains(list.get("any")));
    list.delete("all");
    assert_eq!(list.length(), 0);
    assert_eq!(list.get("last"), &ScratchValue::Text(String::new()));
    list.insert("ignored".into(), "last");
    list.replace("random", "ignored".into());
    list.delete("any");
    assert!(list.items().is_empty());
}

#[test]
fn compact_initializer_keeps_large_list_values_and_final_index_accessible() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../aida-ast/tests/src/monitor-tests-sensing.sb3"
    );
    let json = aida_ast::extractor::read_project_json(fixture).unwrap();
    let project = aida_ast::parser::parse_project(&json).unwrap();
    let source_items = project
        .targets
        .iter()
        .flat_map(|target| target.lists.values())
        .find(|list| list.items.len() == 200_000)
        .expect("fixture should contain its complete 200,000-item list");
    let serialized = serde_json::to_string(&source_items.items).unwrap();
    let list = ScratchList::from_json_bytes(serialized.as_bytes());

    assert_eq!(list.length(), 200_000);
    let source_number =
        |index: usize| ScratchValue::Number(source_items.items[index].as_f64().unwrap());
    assert_eq!(list.get(1.0_f64), &source_number(0));
    assert_eq!(list.get(100_001.0_f64), &source_number(100_000));
    assert_eq!(list.get("last"), &source_number(199_999));
}

#[test]
#[ignore = "manual release-mode microbenchmark"]
fn benchmark_vec_backed_list_operations() {
    use std::{hint::black_box, time::Instant};

    for size in [1_000, 100_000] {
        let seed = (0..size)
            .map(|value| ScratchValue::Number(value as f64))
            .collect::<Vec<_>>();
        let mut list = ScratchList::new(seed);

        let started = Instant::now();
        for value in 0..10_000 {
            black_box(list.add(ScratchValue::Number(value as f64)));
        }
        let append = started.elapsed();

        let started = Instant::now();
        for index in 0..100_000 {
            black_box(list.get((index % size + 1) as f64));
        }
        let indexed_read = started.elapsed();

        let started = Instant::now();
        for index in 0..10_000 {
            list.replace(
                (index % size + 1) as f64,
                ScratchValue::Boolean(index % 2 == 0),
            );
        }
        let replacement = started.elapsed();

        let started = Instant::now();
        for _ in 0..100 {
            black_box(list.item_number(-1.0_f64));
        }
        let search_miss = started.elapsed();

        let mut edits = Vec::new();
        let edit_size = size + 10_000;
        for (label, position) in [("front", 1), ("middle", edit_size / 2), ("tail", edit_size)] {
            let mut edit_list = ScratchList::new(list.items().to_vec());
            let started = Instant::now();
            for _ in 0..100 {
                edit_list.insert(ScratchValue::Text("x".into()), position as f64);
                edit_list.delete(position as f64);
            }
            edits.push(format!("{label}_insert_delete_100={:?}", started.elapsed()));
        }

        println!(
            "size={size}: append_10000={append:?}, read_100000={indexed_read:?}, replace_10000={replacement:?}, search_miss_100={search_miss:?}, {}",
            edits.join(", ")
        );
    }
}
