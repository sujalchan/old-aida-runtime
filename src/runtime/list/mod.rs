use super::{
    CachedListIndex, ScratchInput, ScratchValue, ScratchValueView, operators::to_text,
    scratch_equals, scratch_random, scratch_to_number,
};
use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::{
    fmt,
    sync::{Arc, OnceLock},
};

pub const SCRATCH_LIST_LIMIT: usize = 200_000;

static EMPTY_ITEM: ScratchValue = ScratchValue::Text(String::new());

const NUMERIC_SNAPSHOT_CHUNK_SIZE: usize = 256;
const RENDERER_NUMERIC_SNAPSHOT_MIN_LEN: usize = 1024;

/// Copy-on-write numeric list view used by renderer snapshots.
#[derive(Debug, Clone, PartialEq)]
pub struct NumericListSnapshot {
    chunks: Arc<Vec<Arc<Vec<f64>>>>,
    len: usize,
}

impl NumericListSnapshot {
    pub fn from_numbers(numbers: &[f64]) -> Self {
        Self {
            chunks: Arc::new(
                numbers
                    .chunks(NUMERIC_SNAPSHOT_CHUNK_SIZE)
                    .map(|chunk| Arc::new(chunk.to_vec()))
                    .collect(),
            ),
            len: numbers.len(),
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, index: usize) -> Option<f64> {
        (index < self.len).then(|| {
            self.chunks[index / NUMERIC_SNAPSHOT_CHUNK_SIZE][index % NUMERIC_SNAPSHOT_CHUNK_SIZE]
        })
    }

    fn set(&mut self, index: usize, number: f64) {
        debug_assert!(index < self.len);
        let chunks = Arc::make_mut(&mut self.chunks);
        Arc::make_mut(&mut chunks[index / NUMERIC_SNAPSHOT_CHUNK_SIZE])
            [index % NUMERIC_SNAPSHOT_CHUNK_SIZE] = number;
    }

    fn push(&mut self, number: f64) {
        let chunk_index = self.len / NUMERIC_SNAPSHOT_CHUNK_SIZE;
        let offset = self.len % NUMERIC_SNAPSHOT_CHUNK_SIZE;
        let chunks = Arc::make_mut(&mut self.chunks);
        if offset == 0 {
            chunks.push(Arc::new(Vec::with_capacity(NUMERIC_SNAPSHOT_CHUNK_SIZE)));
        }
        Arc::make_mut(&mut chunks[chunk_index]).push(number);
        self.len += 1;
    }
}

#[derive(Debug, Clone)]
pub struct ScratchList {
    // `items` is authoritative unless the optional compact numeric form is active.
    items: Arc<Vec<ScratchValue>>,
    numeric_items: Option<Arc<Vec<f64>>>,
    // Shared snapshots and generic borrowed reads materialize this cache lazily.
    materialized_items: OnceLock<Arc<Vec<ScratchValue>>>,
    renderer_numeric_items: OnceLock<Arc<NumericListSnapshot>>,
    cached_selectors: OnceLock<Arc<Vec<CachedListIndex>>>,
    // true only when constructors or generated operations establish numeric-only contents.
    all_numbers: bool,
    // finite numeric lists can use exact SIMD equality for numeric item searches.
    all_finite_numbers: bool,
}

/// A synchronous append session for generated numeric batches.
///
/// The list reserves once for the session, and callers may append in smaller
/// chunks while checking scheduler watchdogs between those chunks.
pub struct GeneratedNumericListAppender<'a> {
    storage: GeneratedNumericListStorage<'a>,
    renderer_numbers: Option<NumericListSnapshot>,
    all_finite_numbers: Option<&'a mut bool>,
    start_len: usize,
    appended: usize,
    append_limit: usize,
}

enum GeneratedNumericListStorage<'a> {
    Numbers(&'a mut Vec<f64>),
    Values(&'a mut Vec<ScratchValue>),
    Full,
}

impl GeneratedNumericListAppender<'_> {
    /// Append generated numeric values while maintaining compact-list hints.
    #[inline]
    pub fn add_numbers(&mut self, count: usize, item: impl FnMut() -> f64) -> bool {
        self.add_numbers_inner::<false>(count, item)
    }

    /// Append a chunk of values whose generator is statically known to produce
    /// finite numbers. The caller retains responsibility for that guarantee.
    #[inline]
    pub fn add_finite_numbers(&mut self, count: usize, item: impl FnMut() -> f64) -> bool {
        self.add_numbers_inner::<true>(count, item)
    }

    #[inline]
    fn add_numbers_inner<const ITEMS_FINITE: bool>(
        &mut self,
        count: usize,
        mut item: impl FnMut() -> f64,
    ) -> bool {
        let count = count.min(self.append_limit.saturating_sub(self.appended));
        if count == 0 {
            return false;
        }

        match &mut self.storage {
            GeneratedNumericListStorage::Numbers(numbers) => {
                let start = self.start_len + self.appended;
                let output = unsafe { numbers.as_mut_ptr().add(start) };
                for offset in 0..count {
                    let number = item();
                    unsafe { output.add(offset).write(number) };
                    if !ITEMS_FINITE && let Some(all_finite_numbers) = &mut self.all_finite_numbers
                    {
                        **all_finite_numbers &= number.is_finite();
                    }
                    if let Some(renderer_numbers) = &mut self.renderer_numbers {
                        renderer_numbers.push(number);
                    }
                    self.appended += 1;
                }
            }
            GeneratedNumericListStorage::Values(values) => {
                let start = self.start_len + self.appended;
                let output = unsafe { values.as_mut_ptr().add(start) };
                for offset in 0..count {
                    let number = item();
                    unsafe { output.add(offset).write(ScratchValue::Number(number)) };
                    if !ITEMS_FINITE && let Some(all_finite_numbers) = &mut self.all_finite_numbers
                    {
                        **all_finite_numbers &= number.is_finite();
                    }
                    self.appended += 1;
                }
            }
            GeneratedNumericListStorage::Full => unreachable!("a full appender has no capacity"),
        }

        true
    }
}

impl Drop for GeneratedNumericListAppender<'_> {
    fn drop(&mut self) {
        let len = self.start_len + self.appended;
        match &mut self.storage {
            GeneratedNumericListStorage::Numbers(numbers) => unsafe { numbers.set_len(len) },
            GeneratedNumericListStorage::Values(values) => unsafe { values.set_len(len) },
            GeneratedNumericListStorage::Full => {}
        }
    }
}

impl ScratchList {
    pub fn new(items: Vec<ScratchValue>) -> Self {
        let all_numbers = items
            .iter()
            .all(|item| matches!(item, ScratchValue::Number(_)));
        let all_finite_numbers = all_numbers
            && items
                .iter()
                .all(|item| matches!(item, ScratchValue::Number(number) if number.is_finite()));
        Self {
            items: Arc::new(items),
            numeric_items: None,
            materialized_items: OnceLock::new(),
            renderer_numeric_items: OnceLock::new(),
            cached_selectors: OnceLock::new(),
            all_numbers,
            all_finite_numbers,
        }
    }

    /// Build a list from the compact JSON initializer emitted by aida-ast.
    pub fn from_json(json: &str) -> Self {
        Self::from_json_bytes(json.as_bytes())
    }

    /// Build a list from a compact JSON byte slice stored beside generated code.
    pub fn from_json_bytes(json: &[u8]) -> Self {
        let items: Vec<ScratchValue> = serde_json::from_slice(json)
            .expect("generated Scratch list initializer must contain a JSON array");
        Self::new(items)
    }

    fn current_items(&self) -> &Arc<Vec<ScratchValue>> {
        match &self.numeric_items {
            Some(numbers) => self.materialized_items.get_or_init(|| {
                Arc::new(numbers.iter().copied().map(ScratchValue::Number).collect())
            }),
            None => &self.items,
        }
    }

    // General mutations switch back to the dynamic representation. Cached
    // snapshots remain independent, and read-only access can materialize lazily.
    fn materialize_for_mutation(&mut self) {
        self.cached_selectors.take();
        self.renderer_numeric_items.take();
        if let Some(numbers) = self.numeric_items.take() {
            self.items = self.materialized_items.take().unwrap_or_else(|| {
                Arc::new(numbers.iter().copied().map(ScratchValue::Number).collect())
            });
        }
    }

    pub fn items(&self) -> &[ScratchValue] {
        self.current_items()
    }

    pub fn shared_items(&self) -> Arc<Vec<ScratchValue>> {
        Arc::clone(self.current_items())
    }

    pub(crate) fn shared_renderer_numeric_items(&self) -> Option<Arc<NumericListSnapshot>> {
        let numbers = self.numeric_items.as_ref()?;
        if numbers.len() < RENDERER_NUMERIC_SNAPSHOT_MIN_LEN {
            return None;
        }
        Some(Arc::clone(self.renderer_numeric_items.get_or_init(|| {
            Arc::new(NumericListSnapshot::from_numbers(numbers))
        })))
    }

    fn update_renderer_numeric_item(&mut self, index: usize, number: f64) {
        if let Some(snapshot) = self.renderer_numeric_items.take() {
            let mut snapshot = Arc::unwrap_or_clone(snapshot);
            snapshot.set(index, number);
            let _ = self.renderer_numeric_items.set(Arc::new(snapshot));
        }
    }

    #[cfg(test)]
    pub(crate) fn has_materialized_numeric_items(&self) -> bool {
        self.numeric_items.is_some() && self.materialized_items.get().is_some()
    }

    pub(crate) fn shares_storage_with(&self, other: &Self) -> bool {
        match (&self.numeric_items, &other.numeric_items) {
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            (None, None) => Arc::ptr_eq(&self.items, &other.items),
            _ => false,
        }
    }

    pub fn add(&mut self, item: ScratchValue) -> bool {
        self.materialize_for_mutation();
        if self.items.len() >= SCRATCH_LIST_LIMIT {
            return false;
        }
        self.all_numbers &= matches!(&item, ScratchValue::Number(_));
        self.all_finite_numbers &=
            matches!(&item, ScratchValue::Number(number) if number.is_finite());
        Arc::make_mut(&mut self.items).push(item);
        true
    }

    // reserve one shared vector mutation for batches emitted from safe warp loops.
    pub fn add_generated(&mut self, count: usize, mut item: impl FnMut() -> ScratchValue) -> bool {
        self.materialize_for_mutation();
        let count = count.min(SCRATCH_LIST_LIMIT.saturating_sub(self.items.len()));
        if count == 0 {
            return false;
        }
        let mut all_numbers = self.all_numbers;
        let mut all_finite_numbers = self.all_finite_numbers;
        let items = Arc::make_mut(&mut self.items);
        items.reserve(count);
        for _ in 0..count {
            let value = item();
            all_numbers &= matches!(value, ScratchValue::Number(_));
            all_finite_numbers &=
                matches!(value, ScratchValue::Number(number) if number.is_finite());
            items.push(value);
        }
        self.all_numbers = all_numbers;
        self.all_finite_numbers = all_finite_numbers;
        true
    }

    // append statically numeric reporters without per-item dynamic type checks.
    pub fn add_generated_numbers(&mut self, count: usize, mut item: impl FnMut() -> f64) -> bool {
        self.add_generated_numbers_inner::<false>(count, &mut item)
    }

    /// Append generated values whose inputs are statically proven finite.
    /// This avoids rechecking finiteness for every element while retaining any
    /// non-finite state already present in the list.
    #[inline]
    pub fn add_generated_finite_numbers(
        &mut self,
        count: usize,
        mut item: impl FnMut() -> f64,
    ) -> bool {
        self.add_generated_numbers_inner::<true>(count, &mut item)
    }

    /// Borrow generated numeric storage once across a scheduler-bounded batch.
    /// The closure may append in chunks, but must not yield to another script
    /// until it returns. Finite values keep compact-number metadata valid.
    #[inline]
    pub fn with_generated_finite_number_appender<R>(
        &mut self,
        maximum_count: usize,
        run: impl FnOnce(&mut GeneratedNumericListAppender<'_>) -> R,
    ) -> R {
        let append_limit = maximum_count.min(SCRATCH_LIST_LIMIT.saturating_sub(self.length()));
        if append_limit == 0 {
            let mut appender = GeneratedNumericListAppender {
                storage: GeneratedNumericListStorage::Full,
                renderer_numbers: None,
                all_finite_numbers: None,
                start_len: 0,
                appended: 0,
                append_limit: 0,
            };
            return run(&mut appender);
        }

        if self.all_numbers {
            self.cached_selectors.take();
            self.materialized_items.take();
            let renderer_numbers = self.renderer_numeric_items.take().map(Arc::unwrap_or_clone);
            if self.numeric_items.is_none() {
                let numbers = self
                    .items
                    .iter()
                    .map(|item| match item {
                        ScratchValue::Number(number) => *number,
                        _ => unreachable!("numeric list invariant"),
                    })
                    .collect();
                self.numeric_items = Some(Arc::new(numbers));
                self.items = Arc::new(Vec::new());
            }
            let numbers = Arc::make_mut(self.numeric_items.as_mut().unwrap());
            let start_len = numbers.len();
            numbers.reserve(append_limit);
            let mut appender = GeneratedNumericListAppender {
                storage: GeneratedNumericListStorage::Numbers(numbers),
                renderer_numbers,
                all_finite_numbers: Some(&mut self.all_finite_numbers),
                start_len,
                appended: 0,
                append_limit,
            };
            let result = run(&mut appender);
            let renderer_numbers = appender.renderer_numbers.take();
            drop(appender);
            if let Some(renderer_numbers) = renderer_numbers {
                let _ = self.renderer_numeric_items.set(Arc::new(renderer_numbers));
            }
            result
        } else {
            self.materialize_for_mutation();
            let items = Arc::make_mut(&mut self.items);
            let start_len = items.len();
            items.reserve(append_limit);
            let mut appender = GeneratedNumericListAppender {
                storage: GeneratedNumericListStorage::Values(items),
                renderer_numbers: None,
                all_finite_numbers: None,
                start_len,
                appended: 0,
                append_limit,
            };
            let result = run(&mut appender);
            drop(appender);
            result
        }
    }

    fn add_generated_numbers_inner<const ITEMS_FINITE: bool>(
        &mut self,
        count: usize,
        item: &mut impl FnMut() -> f64,
    ) -> bool {
        if self.all_numbers {
            self.cached_selectors.take();
            self.materialized_items.take();
            let current_len = self.length();
            let count = count.min(SCRATCH_LIST_LIMIT.saturating_sub(current_len));
            if count == 0 {
                return false;
            }
            let mut renderer_numbers = self.renderer_numeric_items.take().map(Arc::unwrap_or_clone);
            if self.numeric_items.is_none() {
                let numbers = self
                    .items
                    .iter()
                    .map(|item| match item {
                        ScratchValue::Number(number) => *number,
                        _ => unreachable!("numeric list invariant"),
                    })
                    .collect();
                self.numeric_items = Some(Arc::new(numbers));
                self.items = Arc::new(Vec::new());
            }
            let numbers = Arc::make_mut(self.numeric_items.as_mut().unwrap());
            let old_len = numbers.len();
            numbers.reserve(count);
            let output = unsafe { numbers.as_mut_ptr().add(old_len) };
            if let Some(renderer_numbers) = &mut renderer_numbers {
                for index in 0..count {
                    let number = item();
                    if !ITEMS_FINITE {
                        self.all_finite_numbers &= number.is_finite();
                    }
                    unsafe { output.add(index).write(number) };
                    renderer_numbers.push(number);
                }
            } else {
                for index in 0..count {
                    let number = item();
                    if !ITEMS_FINITE {
                        self.all_finite_numbers &= number.is_finite();
                    }
                    unsafe { output.add(index).write(number) };
                }
            }
            // Every written value is an f64, so a panic in the generator leaves
            // the original Vec length and its initialized prefix intact.
            unsafe { numbers.set_len(old_len + count) };
            if let Some(renderer_numbers) = renderer_numbers {
                let _ = self.renderer_numeric_items.set(Arc::new(renderer_numbers));
            }
            return true;
        }

        self.materialize_for_mutation();
        let count = count.min(SCRATCH_LIST_LIMIT.saturating_sub(self.items.len()));
        if count == 0 {
            return false;
        }
        let all_numbers = self.all_numbers;
        let mut all_finite_numbers = self.all_finite_numbers;
        let items = Arc::make_mut(&mut self.items);
        items.reserve(count);
        for _ in 0..count {
            let number = item();
            if !ITEMS_FINITE {
                all_finite_numbers &= number.is_finite();
            }
            items.push(ScratchValue::Number(number));
        }
        self.all_numbers = all_numbers;
        self.all_finite_numbers = all_finite_numbers;
        true
    }

    pub fn replace_all(&mut self, items: Vec<ScratchValue>) {
        self.materialize_for_mutation();
        self.all_numbers = items
            .iter()
            .all(|item| matches!(item, ScratchValue::Number(_)));
        self.all_finite_numbers = self.all_numbers
            && items
                .iter()
                .all(|item| matches!(item, ScratchValue::Number(number) if number.is_finite()));
        self.items = Arc::new(items);
    }

    pub fn delete(&mut self, index: impl ScratchInput) -> bool {
        self.materialize_for_mutation();
        match resolve_index(index, self.items.len(), true) {
            ListIndex::All => self.delete_all(),
            ListIndex::Item(index) => {
                Arc::make_mut(&mut self.items).remove(index);
                if self.items.is_empty() {
                    self.all_numbers = true;
                    self.all_finite_numbers = true;
                }
                true
            }
            ListIndex::Invalid => false,
        }
    }

    // clear shared lists without cloning saved items, and keep the allocation for a refill.
    pub fn delete_all(&mut self) -> bool {
        self.renderer_numeric_items.take();
        if let Some(numbers) = &mut self.numeric_items {
            self.cached_selectors.take();
            self.materialized_items.take();
            if numbers.is_empty() {
                self.all_numbers = true;
                self.all_finite_numbers = true;
                return false;
            }
            Arc::make_mut(numbers).clear();
            self.all_numbers = true;
            self.all_finite_numbers = true;
            return true;
        }

        self.materialize_for_mutation();
        if self.items.is_empty() {
            self.all_numbers = true;
            self.all_finite_numbers = true;
            return false;
        }
        if let Some(items) = Arc::get_mut(&mut self.items) {
            if self.all_numbers {
                // numeric values own no resources, so their drop glue can be skipped safely.
                unsafe { items.set_len(0) };
            } else {
                items.clear();
            }
        } else {
            let capacity = self.items.capacity().min(SCRATCH_LIST_LIMIT);
            self.items = Arc::new(Vec::with_capacity(capacity));
        }
        self.all_numbers = true;
        self.all_finite_numbers = true;
        true
    }

    pub fn insert(&mut self, item: ScratchValue, index: impl ScratchInput) -> bool {
        self.materialize_for_mutation();
        let index = match resolve_index(index, self.items.len().saturating_add(1), false) {
            ListIndex::Item(index) => index,
            ListIndex::All | ListIndex::Invalid => return false,
        };
        if index >= SCRATCH_LIST_LIMIT {
            return false;
        }
        self.all_finite_numbers &=
            matches!(&item, ScratchValue::Number(number) if number.is_finite());
        self.all_numbers &= matches!(&item, ScratchValue::Number(_));
        let items = Arc::make_mut(&mut self.items);
        items.insert(index, item);
        if items.len() > SCRATCH_LIST_LIMIT {
            items.pop();
        }
        true
    }

    pub fn replace(&mut self, index: impl ScratchInput, item: ScratchValue) -> bool {
        self.materialize_for_mutation();
        let ListIndex::Item(index) = resolve_index(index, self.items.len(), false) else {
            return false;
        };
        if self.items[index] == item {
            return false;
        }
        self.all_numbers &= matches!(&item, ScratchValue::Number(_));
        self.all_finite_numbers &=
            matches!(&item, ScratchValue::Number(number) if number.is_finite());
        Arc::make_mut(&mut self.items)[index] = item;
        true
    }

    /// replace a numeric one-based item without building dynamic Scratch values.
    #[inline]
    pub fn replace_number(&mut self, index: f64, item: f64) -> bool {
        if self.all_finite_numbers {
            let Some(index) = resolve_numeric_index(index, self.length()) else {
                return false;
            };
            let previous = if let Some(numbers) = &self.numeric_items {
                numbers[index]
            } else {
                let ScratchValue::Number(number) = self.items[index] else {
                    unreachable!("finite numeric list invariant");
                };
                number
            };
            if previous == item {
                return false;
            }
            if self.numeric_items.is_none() {
                let numbers = self
                    .items
                    .iter()
                    .map(|item| match item {
                        ScratchValue::Number(number) => *number,
                        _ => unreachable!("finite numeric list invariant"),
                    })
                    .collect();
                self.numeric_items = Some(Arc::new(numbers));
                self.items = Arc::new(Vec::new());
            }
            Arc::make_mut(self.numeric_items.as_mut().unwrap())[index] = item;
            if let Some(items) = self.materialized_items.get_mut() {
                Arc::make_mut(items)[index] = ScratchValue::Number(item);
            }
            self.all_finite_numbers &= item.is_finite();
            self.update_renderer_numeric_item(index, item);
            return true;
        }

        self.materialize_for_mutation();
        let Some(index) = resolve_numeric_index(index, self.items.len()) else {
            return false;
        };
        if matches!(&self.items[index], ScratchValue::Number(current) if *current == item) {
            return false;
        }
        Arc::make_mut(&mut self.items)[index] = ScratchValue::Number(item);
        self.all_finite_numbers &= item.is_finite();
        true
    }

    /// replace a numeric slot and compare the value read back using one resolved index.
    #[inline]
    pub fn replace_number_and_equals(
        &mut self,
        index: f64,
        item: f64,
        expected: f64,
    ) -> (bool, bool) {
        if self.all_numbers {
            let Some(index) = resolve_numeric_index(index, self.length()) else {
                return (false, false);
            };
            let previous = if let Some(numbers) = &self.numeric_items {
                numbers[index]
            } else {
                let ScratchValue::Number(number) = self.items[index] else {
                    unreachable!("numeric list invariant");
                };
                number
            };
            let changed = previous != item;
            let actual = if changed {
                if self.numeric_items.is_none() {
                    let numbers = self
                        .items
                        .iter()
                        .map(|item| match item {
                            ScratchValue::Number(number) => *number,
                            _ => unreachable!("numeric list invariant"),
                        })
                        .collect();
                    self.numeric_items = Some(Arc::new(numbers));
                    self.items = Arc::new(Vec::new());
                }
                Arc::make_mut(self.numeric_items.as_mut().unwrap())[index] = item;
                if let Some(items) = self.materialized_items.get_mut() {
                    Arc::make_mut(items)[index] = ScratchValue::Number(item);
                }
                self.all_finite_numbers &= item.is_finite();
                self.update_renderer_numeric_item(index, item);
                item
            } else {
                previous
            };
            return (changed, numeric_equals(actual, expected));
        }

        self.materialize_for_mutation();
        let Some(index) = resolve_numeric_index(index, self.items.len()) else {
            return (false, false);
        };
        let changed =
            !matches!(&self.items[index], ScratchValue::Number(current) if *current == item);
        if changed {
            Arc::make_mut(&mut self.items)[index] = ScratchValue::Number(item);
            self.all_finite_numbers &= item.is_finite();
        }
        let matches = match &self.items[index] {
            ScratchValue::Number(actual) => scratch_equals(*actual, expected),
            _ => unreachable!("numeric replacement stores a number before reading it back"),
        };
        (changed, matches)
    }

    pub(crate) fn batch_placeholder() -> Self {
        static EMPTY: OnceLock<Arc<Vec<ScratchValue>>> = OnceLock::new();
        Self {
            items: Arc::clone(EMPTY.get_or_init(|| Arc::new(Vec::new()))),
            numeric_items: None,
            materialized_items: OnceLock::new(),
            renderer_numeric_items: OnceLock::new(),
            cached_selectors: OnceLock::new(),
            all_numbers: true,
            all_finite_numbers: true,
        }
    }

    /// Borrow the authoritative items once for a synchronous generated batch.
    pub fn with_numeric_replacements<R>(
        &mut self,
        run: impl FnOnce(&mut NumericListReplacements<'_>) -> R,
    ) -> R {
        self.cached_selectors.take();
        self.renderer_numeric_items.take();
        if self.all_numbers {
            if self.numeric_items.is_none() {
                let numbers = self
                    .items
                    .iter()
                    .map(|item| match item {
                        ScratchValue::Number(number) => *number,
                        _ => unreachable!("numeric list invariant"),
                    })
                    .collect();
                self.numeric_items = Some(Arc::new(numbers));
            }
            self.materialized_items.take();
            let items = Arc::make_mut(self.numeric_items.as_mut().unwrap());
            run(&mut NumericListReplacements::Numbers(
                items,
                &mut self.all_finite_numbers,
            ))
        } else {
            self.materialize_for_mutation();
            let items = Arc::make_mut(&mut self.items);
            run(&mut NumericListReplacements::Values(items))
        }
    }

    /// compare a numeric list item while keeping the general Scratch path available.
    #[inline]
    pub fn equals_number(&self, index: f64, value: f64) -> Option<bool> {
        if !self.all_numbers {
            return None;
        }
        let index = resolve_numeric_index(index, self.length())?;
        match &self.current_items()[index] {
            ScratchValue::Number(item) => Some(
                item.partial_cmp(&value)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .is_eq(),
            ),
            _ => None,
        }
    }

    #[inline]
    pub fn get(&self, index: impl ScratchInput) -> &ScratchValue {
        if let ScratchValueView::Number(number) = index.scratch_view() {
            return resolve_numeric_index(number, self.length())
                .and_then(|index| self.current_items().get(index))
                .unwrap_or(&EMPTY_ITEM);
        }
        match resolve_index(index, self.length(), false) {
            ListIndex::Item(index) => &self.current_items()[index],
            ListIndex::All | ListIndex::Invalid => &EMPTY_ITEM,
        }
    }

    /// Read an owned item without materializing a compact numeric list.
    #[inline]
    pub fn get_value(&self, index: impl ScratchInput) -> ScratchValue {
        match resolve_index(index, self.length(), false) {
            ListIndex::Item(index) => self
                .numeric_items
                .as_ref()
                .map_or_else(|| self.items[index].clone(), |items| items[index].into()),
            ListIndex::All | ListIndex::Invalid => EMPTY_ITEM.clone(),
        }
    }

    /// Return the original item and a cached selector derived from it. The
    /// cache is dropped on every mutation and never changes list contents.
    #[inline]
    pub fn get_with_cached_selector(
        &self,
        index: impl ScratchInput,
    ) -> (&ScratchValue, CachedListIndex) {
        let item_index = match resolve_index(index, self.length(), false) {
            ListIndex::Item(index) => index,
            ListIndex::All | ListIndex::Invalid => {
                return (&EMPTY_ITEM, CachedListIndex::new(&EMPTY_ITEM));
            }
        };
        let items = self.current_items();
        let selectors = self
            .cached_selectors
            .get_or_init(|| Arc::new(items.iter().map(CachedListIndex::new).collect()));
        (&items[item_index], selectors[item_index])
    }

    #[inline]
    pub fn get_cached_number(&self, index: impl ScratchInput) -> f64 {
        let (_, selector) = self.get_with_cached_selector(index);
        scratch_to_number(selector)
    }

    pub fn length(&self) -> usize {
        self.numeric_items
            .as_ref()
            .map_or_else(|| self.items.len(), |items| items.len())
    }

    pub fn contains(&self, item: impl ScratchInput) -> bool {
        self.current_items()
            .iter()
            .any(|value| scratch_equals(value, &item))
    }

    pub fn item_number(&self, item: impl ScratchInput) -> usize {
        // compare numeric entries directly while preserving scratch fallback for mixed lists.
        let query = item.scratch_view();
        if let (Some(numbers), ScratchValueView::Number(search)) = (&self.numeric_items, query) {
            let index = if self.all_finite_numbers && search.is_finite() {
                numeric_position(numbers, search)
            } else {
                numbers
                    .iter()
                    .position(|candidate| numeric_equals(*candidate, search))
            };
            return index.map_or(0, |index| index + 1);
        }
        let index = if let ScratchValueView::Number(search) = query {
            self.current_items().iter().position(|value| match value {
                ScratchValue::Number(candidate) if !search.is_nan() && !candidate.is_nan() => {
                    *candidate == search
                }
                _ => scratch_equals(value, &item),
            })
        } else {
            self.current_items()
                .iter()
                .position(|value| scratch_equals(value, &item))
        };
        index.map_or(0, |index| index + 1)
    }

    pub fn contents(&self) -> String {
        let single_letter_items = self.current_items().iter().all(|item| match item {
            ScratchValue::Text(text) => text.encode_utf16().count() == 1,
            ScratchValue::Number(_) | ScratchValue::Boolean(_) | ScratchValue::Undefined => false,
        });
        let separator = if single_letter_items { "" } else { " " };
        let mut output = String::new();
        for (index, item) in self.current_items().iter().enumerate() {
            if index != 0 {
                output.push_str(separator);
            }
            output.push_str(&to_text(item.scratch_view()));
        }
        output
    }
}

/// A non-suspending numeric replacement batch over the original list storage.
pub enum NumericListReplacements<'a> {
    Values(&'a mut [ScratchValue]),
    Numbers(&'a mut [f64], &'a mut bool),
}

impl NumericListReplacements<'_> {
    #[inline]
    pub fn replace_number_and_equals(
        &mut self,
        index: f64,
        item: f64,
        expected: f64,
    ) -> (bool, bool) {
        // Saturating conversion truncates positive values, maps NaN/negative
        // to zero and maps infinity beyond the maximum possible Vec length.
        let Some(index) = (index as usize).checked_sub(1) else {
            return (false, false);
        };
        match self {
            Self::Values(items) => {
                let Some(slot) = items.get_mut(index) else {
                    return (false, false);
                };
                let changed = !matches!(slot, ScratchValue::Number(current) if *current == item);
                if changed {
                    *slot = ScratchValue::Number(item);
                }
                let ScratchValue::Number(actual) = slot else {
                    unreachable!("numeric replacement stores a number");
                };
                (changed, numeric_equals(*actual, expected))
            }
            Self::Numbers(items, all_finite_numbers) => {
                let Some(slot) = items.get_mut(index) else {
                    return (false, false);
                };
                let changed = *slot != item;
                if changed {
                    *slot = item;
                    **all_finite_numbers &= item.is_finite();
                }
                (changed, numeric_equals(*slot, expected))
            }
        }
    }
}

#[inline]
fn numeric_equals(actual: f64, expected: f64) -> bool {
    if !actual.is_nan() && !expected.is_nan() {
        actual == expected
    } else {
        scratch_equals(actual, expected)
    }
}

#[inline]
fn numeric_position(numbers: &[f64], search: f64) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    if avx2_available() {
        // The caller proves both operands are finite, so ordered SIMD equality
        // matches the scalar numeric comparison exactly.
        return unsafe { numeric_position_avx2(numbers, search) };
    }
    numbers.iter().position(|&candidate| candidate == search)
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn avx2_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| std::arch::is_x86_feature_detected!("avx2"))
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn numeric_position_avx2(numbers: &[f64], search: f64) -> Option<usize> {
    use std::arch::x86_64::{
        _CMP_EQ_OQ, _mm256_cmp_pd, _mm256_loadu_pd, _mm256_movemask_pd, _mm256_set1_pd,
    };

    let needle = _mm256_set1_pd(search);
    let mut index = 0;
    while index + 16 <= numbers.len() {
        let values0 = unsafe { _mm256_loadu_pd(numbers.as_ptr().add(index)) };
        let values1 = unsafe { _mm256_loadu_pd(numbers.as_ptr().add(index + 4)) };
        let values2 = unsafe { _mm256_loadu_pd(numbers.as_ptr().add(index + 8)) };
        let values3 = unsafe { _mm256_loadu_pd(numbers.as_ptr().add(index + 12)) };
        let mask0 = _mm256_movemask_pd(_mm256_cmp_pd(values0, needle, _CMP_EQ_OQ)) as u32;
        let mask1 = _mm256_movemask_pd(_mm256_cmp_pd(values1, needle, _CMP_EQ_OQ)) as u32;
        let mask2 = _mm256_movemask_pd(_mm256_cmp_pd(values2, needle, _CMP_EQ_OQ)) as u32;
        let mask3 = _mm256_movemask_pd(_mm256_cmp_pd(values3, needle, _CMP_EQ_OQ)) as u32;
        if (mask0 | mask1 | mask2 | mask3) != 0 {
            if mask0 != 0 {
                return Some(index + mask0.trailing_zeros() as usize);
            }
            if mask1 != 0 {
                return Some(index + 4 + mask1.trailing_zeros() as usize);
            }
            if mask2 != 0 {
                return Some(index + 8 + mask2.trailing_zeros() as usize);
            }
            return Some(index + 12 + mask3.trailing_zeros() as usize);
        }
        index += 16;
    }
    while index + 4 <= numbers.len() {
        let values = unsafe { _mm256_loadu_pd(numbers.as_ptr().add(index)) };
        let mask = _mm256_movemask_pd(_mm256_cmp_pd(values, needle, _CMP_EQ_OQ)) as u32;
        if mask != 0 {
            return Some(index + mask.trailing_zeros() as usize);
        }
        index += 4;
    }
    numbers[index..]
        .iter()
        .position(|&candidate| candidate == search)
        .map(|tail| index + tail)
}

impl Default for ScratchList {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl PartialEq for ScratchList {
    fn eq(&self, other: &Self) -> bool {
        self.items() == other.items()
    }
}

impl<'de> serde::Deserialize<'de> for ScratchValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ScratchValueVisitor;

        impl<'de> Visitor<'de> for ScratchValueVisitor {
            type Value = ScratchValue;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a Scratch list item")
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(ScratchValue::Boolean(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(ScratchValue::Number(value as f64))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(ScratchValue::Number(value as f64))
            }

            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E> {
                Ok(ScratchValue::Number(value))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(ScratchValue::Text(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(ScratchValue::Text(value))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(ScratchValue::Number(0.0))
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                while sequence.next_element::<IgnoredAny>()?.is_some() {}
                Ok(ScratchValue::Number(0.0))
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(ScratchValue::Number(0.0))
            }
        }

        deserializer.deserialize_any(ScratchValueVisitor)
    }
}

enum ListIndex {
    Item(usize),
    All,
    Invalid,
}

fn resolve_index(index: impl ScratchInput, length: usize, accept_all: bool) -> ListIndex {
    match index.scratch_view() {
        ScratchValueView::Text("all") if accept_all => ListIndex::All,
        ScratchValueView::Text("last") => length
            .checked_sub(1)
            .map(ListIndex::Item)
            .unwrap_or(ListIndex::Invalid),
        ScratchValueView::Text("random" | "any") if length > 0 => {
            ListIndex::Item(scratch_random(1.0_f64, length as f64) as usize - 1)
        }
        ScratchValueView::Text("all" | "random" | "any") => ListIndex::Invalid,
        value => {
            let number = match value {
                ScratchValueView::Number(number) => number,
                ScratchValueView::Text(text) => scratch_to_number(text),
                ScratchValueView::Boolean(value) => scratch_to_number(value),
                ScratchValueView::Undefined => 0.0,
            };
            if !number.is_finite() {
                return ListIndex::Invalid;
            }
            let index = number.floor();
            if index < 1.0 || index > length as f64 {
                return ListIndex::Invalid;
            }
            ListIndex::Item(index as usize - 1)
        }
    }
}

#[inline]
fn resolve_numeric_index(index: f64, length: usize) -> Option<usize> {
    if !index.is_finite() {
        return None;
    }
    let index = index.floor();
    if index < 1.0 || index > length as f64 {
        return None;
    }
    Some(index as usize - 1)
}

#[cfg(test)]
mod tests;
