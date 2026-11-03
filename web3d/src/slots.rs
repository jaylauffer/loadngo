//! Generational handle tables. The JS side holds plain numbers for meshes, textures, materials
//! and objects; a number for something already destroyed, never created, of another kind, or from
//! another renderer is refused here instead of reaching whatever uses its slot.
//!
//! A handle is `salt << 25 | kind << 22 | generation << 15 | (index + 1)`:
//! - 15 bits of slot index, so a table holds up to 32,767 live values;
//! - 7 bits of generation;
//! - 3 bits of kind ([`Kind`]: mesh, texture, material, object; never 0);
//! - 5 bits of salt, which the renderer is given when it is created, so two live renderers' handles
//!   differ (the JS wrapper gives each live renderer its own salt).
//!
//! So every handle is a positive number below 2^30 (a small integer in V8, never a boxed one), and
//! 0, or any number without a kind, is never a valid handle. A table accepts only handles carrying
//! its own kind and salt. Freeing a slot bumps its generation, and a slot whose generation has
//! reached its last value is retired instead of reused, so no handle is ever issued twice: a stale
//! handle is refused for the life of the table, never aliased to a newer object. Each slot serves
//! 128 values before it retires, so a table issues about 4.2 million handles in its life.

const INDEX_BITS: u32 = 15;
const INDEX_MASK: u32 = (1 << INDEX_BITS) - 1;
const GENERATION_BITS: u32 = 7;
/// The last generation a slot reaches before it retires.
const LAST_GENERATION: u32 = (1 << GENERATION_BITS) - 1;
const KIND_SHIFT: u32 = INDEX_BITS + GENERATION_BITS;
const KIND_BITS: u32 = 3;
const SALT_SHIFT: u32 = KIND_SHIFT + KIND_BITS;
/// How many distinct salts there are (5 bits).
pub const SALTS: u32 = 1 << 5;
/// The bits a table compares whole: kind, salt, and the two top bits, which no handle has.
const TAG_MASK: u32 = !((1 << KIND_SHIFT) - 1);
/// The most slots a table can hold.
pub const MAX_SLOTS: usize = INDEX_MASK as usize;

/// What a handle names. The value is its kind tag; 0 is no kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Kind {
    Mesh = 1,
    Texture = 2,
    Material = 3,
    Object = 4,
}

impl Kind {
    pub const fn name(self) -> &'static str {
        match self {
            Kind::Mesh => "mesh",
            Kind::Texture => "texture",
            Kind::Material => "material",
            Kind::Object => "object",
        }
    }

    /// The kind a handle carries, if any.
    pub const fn of(handle: u32) -> Option<Kind> {
        match (handle >> KIND_SHIFT) & ((1 << KIND_BITS) - 1) {
            1 => Some(Kind::Mesh),
            2 => Some(Kind::Texture),
            3 => Some(Kind::Material),
            4 => Some(Kind::Object),
            _ => None,
        }
    }
}

/// The salt a handle carries.
pub const fn salt_of(handle: u32) -> u32 {
    (handle >> SALT_SHIFT) & (SALTS - 1)
}

struct Entry<T> {
    generation: u32,
    value: Option<T>,
}

pub struct Slots<T> {
    entries: Vec<Entry<T>>,
    free: Vec<u32>,
    live: usize,
    retired: usize,
    /// The kind and salt bits every handle of this table carries.
    tag: u32,
}

impl<T> Slots<T> {
    /// A table of one kind, with salt `salt` (taken modulo [`SALTS`]; the renderer checks it).
    pub const fn new(kind: Kind, salt: u32) -> Self {
        Self {
            entries: Vec::new(),
            free: Vec::new(),
            live: 0,
            retired: 0,
            tag: (salt % SALTS) << SALT_SHIFT | (kind as u32) << KIND_SHIFT,
        }
    }

    /// Stores a value and returns its handle and slot index, or `None` when the table is full.
    pub fn insert(&mut self, value: T) -> Option<(u32, usize)> {
        let index = match self.free.pop() {
            Some(index) => index as usize,
            None => {
                if self.entries.len() >= MAX_SLOTS {
                    return None;
                }
                self.entries.push(Entry {
                    generation: 0,
                    value: None,
                });
                self.entries.len() - 1
            }
        };
        let entry = &mut self.entries[index];
        entry.value = Some(value);
        self.live += 1;
        Some((
            self.tag | entry.generation << INDEX_BITS | (index as u32 + 1),
            index,
        ))
    }

    /// The slot index of a live handle of this table's kind and salt.
    pub fn index_of(&self, handle: u32) -> Option<usize> {
        if handle & TAG_MASK != self.tag {
            return None;
        }
        let index = (handle & INDEX_MASK) as usize;
        let index = index.checked_sub(1)?;
        let entry = self.entries.get(index)?;
        let generation = (handle >> INDEX_BITS) & LAST_GENERATION;
        (entry.value.is_some() && entry.generation == generation).then_some(index)
    }

    pub fn get(&self, handle: u32) -> Option<&T> {
        self.at(self.index_of(handle)?)
    }

    pub fn get_mut(&mut self, handle: u32) -> Option<&mut T> {
        let index = self.index_of(handle)?;
        self.at_mut(index)
    }

    /// The value in a slot, by index.
    pub fn at(&self, index: usize) -> Option<&T> {
        self.entries.get(index)?.value.as_ref()
    }

    pub fn at_mut(&mut self, index: usize) -> Option<&mut T> {
        self.entries.get_mut(index)?.value.as_mut()
    }

    /// Takes the value out of a live slot and frees the slot: its next generation, or retirement.
    fn vacate(&mut self, index: usize) -> Option<T> {
        let entry = &mut self.entries[index];
        let value = entry.value.take()?;
        self.live -= 1;
        if entry.generation == LAST_GENERATION {
            self.retired += 1;
        } else {
            entry.generation += 1;
            self.free.push(index as u32);
        }
        Some(value)
    }

    /// Removes a value; its handle, and any copy of it, stops working for good.
    pub fn remove(&mut self, handle: u32) -> Option<T> {
        let index = self.index_of(handle)?;
        self.vacate(index)
    }

    /// Live values.
    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Slots that have served their last generation and are never reused.
    pub fn retired(&self) -> usize {
        self.retired
    }

    /// Slots ever used: every live index is below this.
    pub fn slot_count(&self) -> usize {
        self.entries.len()
    }

    /// Live values with their slot indices, in slot order. Allocates nothing.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &T)> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.value.as_ref().map(|v| (i, v)))
    }

    /// Removes every value, handing each to `f` (for teardown). Generations carry on, so handles
    /// issued before stay refused afterwards.
    pub fn remove_all(&mut self, mut f: impl FnMut(T)) {
        for index in 0..self.entries.len() {
            if let Some(value) = self.vacate(index) {
                f(value);
            }
        }
    }

    /// Removes every value; handles issued before stay refused.
    pub fn clear(&mut self) {
        self.remove_all(drop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_are_positive_small_and_never_zero() {
        let mut s = Slots::new(Kind::Mesh, 0);
        let a = s.insert("a").unwrap().0;
        let b = s.insert("b").unwrap().0;
        assert!(a != 0 && b != 0 && a != b);
        assert!(a < 1 << 30 && b < 1 << 30);
        assert_eq!(s.get(a), Some(&"a"));
        assert_eq!(s.get(b), Some(&"b"));
        assert_eq!(s.get(0), None);
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn stale_handles_fail_after_their_slot_is_reused() {
        let mut s = Slots::new(Kind::Object, 7);
        let a = s.insert(1).unwrap().0;
        assert_eq!(s.remove(a), Some(1));
        assert_eq!(s.get(a), None);
        assert_eq!(s.remove(a), None, "double destroy is caught");
        let b = s.insert(2).unwrap().0;
        assert_eq!(s.index_of(a), None);
        assert_eq!(s.index_of(b), Some(0), "slot reused");
        assert_ne!(a, b);
        assert_eq!(s.get(b), Some(&2));
        assert_eq!(s.slot_count(), 1);
    }

    #[test]
    fn forged_handles_fail() {
        let mut s = Slots::new(Kind::Object, 7);
        let a = s.insert(()).unwrap().0;
        assert_eq!(s.get(a + 1), None, "index past the end");
        assert_eq!(s.get(a ^ 5 << INDEX_BITS), None, "wrong generation");
        assert_eq!(s.get(u32::MAX), None);
    }

    #[test]
    fn no_handle_is_ever_issued_twice_a_slot_retires_instead_of_wrapping() {
        let mut s = Slots::new(Kind::Object, 7);
        let mut seen = std::collections::HashSet::new();
        let first = s.insert(0).unwrap().0;
        seen.insert(first);
        s.remove(first);
        // 128 generations on slot 0, then it retires and a new slot serves.
        for i in 1..300 {
            let (h, _) = s.insert(i).unwrap();
            assert!(h < 1 << 30);
            assert!(seen.insert(h), "handle {h} issued twice (cycle {i})");
            s.remove(h);
        }
        assert_eq!(s.get(first), None);
        assert_eq!(
            s.remove(first),
            None,
            "the very first handle is still refused"
        );
        assert_eq!(s.retired(), 2, "slots 0 and 1 served 128 objects each");
        assert_eq!(s.slot_count(), 3);
        assert!(s.is_empty());
    }

    #[test]
    fn clearing_keeps_old_handles_refused() {
        let mut s = Slots::new(Kind::Object, 7);
        let a = s.insert('a').unwrap().0;
        let b = s.insert('b').unwrap().0;
        s.clear();
        assert!(s.is_empty());
        let c = s.insert('c').unwrap().0;
        let d = s.insert('d').unwrap().0;
        assert_eq!(s.get(a), None);
        assert_eq!(s.get(b), None);
        assert!(c != a && c != b && d != a && d != b);
        assert_eq!((s.get(c), s.get(d)), (Some(&'c'), Some(&'d')));
    }

    #[test]
    fn iter_and_remove_all_visit_live_values() {
        let mut s = Slots::new(Kind::Object, 7);
        let a = s.insert('a').unwrap().0;
        let _b = s.insert('b').unwrap().0;
        let _c = s.insert('c').unwrap().0;
        s.remove(a);
        let seen: Vec<_> = s.iter().map(|(i, v)| (i, *v)).collect();
        assert_eq!(seen, [(1, 'b'), (2, 'c')]);
        let mut removed = Vec::new();
        s.remove_all(|v| removed.push(v));
        assert_eq!(removed, ['b', 'c']);
        assert!(s.is_empty() && s.slot_count() == 3);
    }

    #[test]
    fn a_handle_of_another_kind_or_salt_is_refused() {
        let mut meshes = Slots::new(Kind::Mesh, 3);
        let mut materials = Slots::new(Kind::Material, 3);
        let mut other = Slots::new(Kind::Mesh, 4);
        let m = meshes.insert('m').unwrap().0;
        let t = materials.insert('t').unwrap().0;
        let o = other.insert('o').unwrap().0;
        // Each table's first slot: only the tag tells them apart.
        assert_eq!(m & INDEX_MASK, t & INDEX_MASK);
        assert_eq!(meshes.get(t), None, "a material handle is not a mesh");
        assert_eq!(materials.get(m), None, "a mesh handle is not a material");
        assert_eq!(meshes.get(o), None, "another renderer's mesh");
        assert_eq!(other.get(m), None);
        assert_eq!(
            (Kind::of(m), Kind::of(t)),
            (Some(Kind::Mesh), Some(Kind::Material))
        );
        assert_eq!((salt_of(m), salt_of(o)), (3, 4));
        for small in 1..=64 {
            assert_eq!(meshes.get(small), None, "a bare small number has no kind");
        }
        assert_eq!(Kind::of(m & !TAG_MASK), None);
        assert!(m < 1 << 30 && t < 1 << 30 && o < 1 << 30);
        assert_eq!(
            materials.remove(m),
            None,
            "destroying with a handle of another kind fails"
        );
        assert_eq!(materials.get(t), Some(&'t'));
    }

    #[test]
    fn the_largest_handle_is_below_two_to_the_thirty() {
        let mut s = Slots::new(Kind::Object, SALTS - 1);
        let h = s.insert(()).unwrap().0;
        let largest = h | INDEX_MASK | LAST_GENERATION << INDEX_BITS;
        assert!(largest < 1 << 30);
        assert_eq!(Kind::of(largest), Some(Kind::Object));
        assert_eq!(salt_of(largest), SALTS - 1);
    }
}
