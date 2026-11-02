//! Positions and compact lookup containers shared by the loaders.

/// A world position in metres: x east, y north, z up.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    /// Builds a position.
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    /// Straight-line distance.
    pub fn dist(self, o: Vec3) -> f32 {
        let (dx, dy, dz) = (self.x - o.x, self.y - o.y, self.z - o.z);
        (dx * dx + dy * dy + dz * dz).sqrt()
    }

    /// Horizontal (x/y) distance; z is ignored.
    pub fn dist2d(self, o: Vec3) -> f32 {
        self.dist2d_sq(o).sqrt()
    }

    /// Squared horizontal distance.
    pub fn dist2d_sq(self, o: Vec3) -> f32 {
        let (dx, dy) = (self.x - o.x, self.y - o.y);
        dx * dx + dy * dy
    }
}

/// Immutable `u32 -> T` map stored as two parallel sorted vectors (binary search, no hashing,
/// no per-entry overhead).
#[derive(Debug)]
pub struct IdMap<T> {
    ids: Vec<u32>,
    vals: Vec<T>,
}

impl<T> Default for IdMap<T> {
    fn default() -> Self {
        Self {
            ids: Vec::new(),
            vals: Vec::new(),
        }
    }
}

impl<T> IdMap<T> {
    /// Builds the map; on duplicate ids the first entry wins.
    pub fn build(mut entries: Vec<(u32, T)>) -> Self {
        entries.sort_by_key(|e| e.0); // stable: keeps the first of equal ids first
        entries.dedup_by_key(|e| e.0);
        let (ids, vals) = entries.into_iter().unzip();
        Self { ids, vals }
    }

    /// The value for `id`.
    pub fn get(&self, id: u32) -> Option<&T> {
        self.ids.binary_search(&id).ok().map(|i| &self.vals[i])
    }

    /// Mutable access to the value for `id` (load-time fix-ups only).
    pub(crate) fn get_mut(&mut self, id: u32) -> Option<&mut T> {
        self.ids.binary_search(&id).ok().map(|i| &mut self.vals[i])
    }

    /// Whether `id` is present.
    pub fn contains(&self, id: u32) -> bool {
        self.ids.binary_search(&id).is_ok()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether the map is empty.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// All values in ascending id order.
    pub fn values(&self) -> &[T] {
        &self.vals
    }

    /// `(id, value)` pairs in ascending id order.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &T)> {
        self.ids.iter().copied().zip(self.vals.iter())
    }

    /// Heap bytes held by the two vectors (not including heap owned by the values).
    pub fn shallow_bytes(&self) -> usize {
        self.ids.capacity() * 4 + self.vals.capacity() * std::mem::size_of::<T>()
    }
}

/// Many strings keyed by `u32` in one arena: sorted ids, offsets and a single `String`.
#[derive(Debug, Default)]
pub struct StrTable {
    ids: Vec<u32>,
    /// `offs[i]..offs[i + 1]` is the text of `ids[i]`.
    offs: Vec<u32>,
    arena: String,
}

impl StrTable {
    /// Builds the table; on duplicate ids the first entry wins.
    pub fn build<S: AsRef<str>>(mut entries: Vec<(u32, S)>) -> Self {
        entries.sort_by_key(|e| e.0);
        entries.dedup_by_key(|e| e.0);
        let mut ids = Vec::with_capacity(entries.len());
        let mut offs = Vec::with_capacity(entries.len() + 1);
        let mut arena = String::with_capacity(entries.iter().map(|e| e.1.as_ref().len()).sum());
        offs.push(0);
        for (id, s) in &entries {
            ids.push(*id);
            arena.push_str(s.as_ref());
            offs.push(arena.len() as u32);
        }
        Self { ids, offs, arena }
    }

    /// The text for `id`.
    pub fn get(&self, id: u32) -> Option<&str> {
        let i = self.ids.binary_search(&id).ok()?;
        Some(&self.arena[self.offs[i] as usize..self.offs[i + 1] as usize])
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Heap bytes held.
    pub fn bytes(&self) -> usize {
        self.ids.capacity() * 4 + self.offs.capacity() * 4 + self.arena.capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vec3_distances() {
        let a = Vec3::new(0.0, 0.0, 0.0);
        let b = Vec3::new(3.0, 4.0, 12.0);
        assert_eq!(a.dist2d(b), 5.0);
        assert_eq!(a.dist(b), 13.0);
    }

    #[test]
    fn idmap_first_duplicate_wins() {
        let m = IdMap::build(vec![(5, "a"), (2, "b"), (5, "c")]);
        assert_eq!(m.get(5), Some(&"a"));
        assert_eq!(m.get(2), Some(&"b"));
        assert_eq!(m.get(3), None);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn strtable_roundtrip() {
        let t = StrTable::build(vec![(9, "nine"), (1, "one"), (4, ""), (1, "dup")]);
        assert_eq!(t.get(1), Some("one"));
        assert_eq!(t.get(4), Some(""));
        assert_eq!(t.get(9), Some("nine"));
        assert_eq!(t.get(2), None);
        assert_eq!(t.len(), 3);
    }
}
