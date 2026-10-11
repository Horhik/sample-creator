//! Edits as reversible changes, applied to the in-memory markup and the store at
//! once. Every user action is one group, so undo reverts it as a whole (a drag is
//! one action, not one per mouse move).

use anyhow::Result;

use super::{Id, Marker, Markup, Region};

/// `None → Some` adds, `Some → None` removes, `Some → Some` updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Marker {
        before: Option<Marker>,
        after: Option<Marker>,
    },
    Region {
        before: Option<Region>,
        after: Option<Region>,
    },
}

impl Change {
    pub fn add_marker(m: Marker) -> Self {
        Change::Marker {
            before: None,
            after: Some(m),
        }
    }

    pub fn add_region(r: Region) -> Self {
        Change::Region {
            before: None,
            after: Some(r),
        }
    }

    fn inverse(&self) -> Self {
        match self.clone() {
            Change::Marker { before, after } => Change::Marker {
                before: after,
                after: before,
            },
            Change::Region { before, after } => Change::Region {
                before: after,
                after: before,
            },
        }
    }
}

/// Persistence used by [`History`]. Saving an item with id 0 assigns a new id.
pub trait Store {
    fn save_marker(&self, m: &Marker) -> Result<Id>;
    fn delete_marker(&self, id: Id) -> Result<()>;
    fn save_region(&self, r: &Region) -> Result<Id>;
    fn delete_region(&self, id: Id) -> Result<()>;
    /// Runs `f` in one transaction.
    fn transaction(&self, f: &mut dyn FnMut() -> Result<()>) -> Result<()>;
}

#[derive(Default)]
pub struct History {
    undo: Vec<Vec<Change>>,
    redo: Vec<Vec<Change>>,
}

impl History {
    /// Applies a user action. Returns the changes as stored (new items get their ids),
    /// so callers can e.g. select a freshly created region.
    pub fn apply(
        &mut self,
        markup: &mut Markup,
        store: &dyn Store,
        changes: Vec<Change>,
    ) -> Result<Vec<Change>> {
        let applied = apply_all(markup, store, changes)?;
        if !applied.is_empty() {
            self.undo.push(applied.clone());
            self.redo.clear();
        }
        Ok(applied)
    }

    pub fn undo(&mut self, markup: &mut Markup, store: &dyn Store) -> Result<bool> {
        let Some(group) = self.undo.pop() else {
            return Ok(false);
        };
        let inverse = group.iter().rev().map(Change::inverse).collect();
        let applied = apply_all(markup, store, inverse)?;
        // Re-inverting keeps ids that a re-insert may have produced.
        self.redo
            .push(applied.iter().rev().map(Change::inverse).collect());
        Ok(true)
    }

    pub fn redo(&mut self, markup: &mut Markup, store: &dyn Store) -> Result<bool> {
        let Some(group) = self.redo.pop() else {
            return Ok(false);
        };
        let applied = apply_all(markup, store, group)?;
        self.undo.push(applied);
        Ok(true)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

/// Undo for edits of whole regions made outside a file's markup, e.g. on the
/// processing page where regions of every recording are edited. Each entry is
/// the region before and after one action.
#[derive(Default)]
pub struct RegionHistory {
    undo: Vec<(Region, Region)>,
    redo: Vec<(Region, Region)>,
}

impl RegionHistory {
    /// Saves `after` and remembers how to go back to `before`.
    pub fn apply(&mut self, store: &dyn Store, before: Region, after: Region) -> Result<()> {
        if before == after {
            return Ok(());
        }
        store.save_region(&after)?;
        self.undo.push((before, after));
        self.redo.clear();
        Ok(())
    }

    /// Restores the previous state; returns the region as saved.
    pub fn undo(&mut self, store: &dyn Store) -> Result<Option<Region>> {
        let Some((before, after)) = self.undo.pop() else {
            return Ok(None);
        };
        store.save_region(&before)?;
        self.redo.push((before.clone(), after));
        Ok(Some(before))
    }

    pub fn redo(&mut self, store: &dyn Store) -> Result<Option<Region>> {
        let Some((before, after)) = self.redo.pop() else {
            return Ok(None);
        };
        store.save_region(&after)?;
        self.undo.push((before, after.clone()));
        Ok(Some(after))
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Forgets a region that was deleted elsewhere, so undo cannot resurrect it.
    pub fn forget(&mut self, id: Id) {
        self.undo.retain(|(b, _)| b.id != id);
        self.redo.retain(|(b, _)| b.id != id);
    }
}

fn apply_all(markup: &mut Markup, store: &dyn Store, changes: Vec<Change>) -> Result<Vec<Change>> {
    let mut applied = Vec::with_capacity(changes.len());
    let mut pending = changes.into_iter();
    // Write the store first; touch memory only once the transaction committed.
    store.transaction(&mut || {
        for change in pending.by_ref() {
            applied.push(persist(store, change)?);
        }
        Ok(())
    })?;
    for change in &applied {
        apply_in_memory(markup, change);
    }
    markup.sort();
    Ok(applied)
}

fn persist(store: &dyn Store, change: Change) -> Result<Change> {
    Ok(match change {
        Change::Marker { before, after } => {
            let after = match after {
                Some(mut m) => {
                    m.id = store.save_marker(&m)?;
                    Some(m)
                }
                None => {
                    if let Some(b) = &before {
                        store.delete_marker(b.id)?;
                    }
                    None
                }
            };
            Change::Marker { before, after }
        }
        Change::Region { before, after } => {
            let after = match after {
                Some(mut r) => {
                    r.id = store.save_region(&r)?;
                    Some(r)
                }
                None => {
                    if let Some(b) = &before {
                        store.delete_region(b.id)?;
                    }
                    None
                }
            };
            Change::Region { before, after }
        }
    })
}

fn apply_in_memory(markup: &mut Markup, change: &Change) {
    match change {
        Change::Marker { before, after } => {
            if let Some(b) = before {
                markup.markers.retain(|m| m.id != b.id);
            }
            if let Some(a) = after {
                markup.markers.retain(|m| m.id != a.id);
                markup.markers.push(a.clone());
            }
        }
        Change::Region { before, after } => {
            if let Some(b) = before {
                markup.regions.retain(|r| r.id != b.id);
            }
            if let Some(a) = after {
                markup.regions.retain(|r| r.id != a.id);
                markup.regions.push(a.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ChannelMode;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// In-memory store that can be told to fail, to check atomicity.
    #[derive(Default)]
    struct MemStore {
        markers: RefCell<BTreeMap<Id, Marker>>,
        regions: RefCell<BTreeMap<Id, Region>>,
        next: RefCell<Id>,
        fail_regions: bool,
    }

    impl MemStore {
        fn id(&self, id: Id) -> Id {
            if id != 0 {
                return id;
            }
            *self.next.borrow_mut() += 1;
            *self.next.borrow()
        }
    }

    impl Store for MemStore {
        fn save_marker(&self, m: &Marker) -> Result<Id> {
            let id = self.id(m.id);
            self.markers
                .borrow_mut()
                .insert(id, Marker { id, ..m.clone() });
            Ok(id)
        }
        fn delete_marker(&self, id: Id) -> Result<()> {
            self.markers.borrow_mut().remove(&id);
            Ok(())
        }
        fn save_region(&self, r: &Region) -> Result<Id> {
            anyhow::ensure!(!self.fail_regions, "disk full");
            let id = self.id(r.id);
            self.regions
                .borrow_mut()
                .insert(id, Region { id, ..r.clone() });
            Ok(id)
        }
        fn delete_region(&self, id: Id) -> Result<()> {
            self.regions.borrow_mut().remove(&id);
            Ok(())
        }
        fn transaction(&self, f: &mut dyn FnMut() -> Result<()>) -> Result<()> {
            let snapshot = (self.markers.borrow().clone(), self.regions.borrow().clone());
            let result = f();
            if result.is_err() {
                *self.markers.borrow_mut() = snapshot.0;
                *self.regions.borrow_mut() = snapshot.1;
            }
            result
        }
    }

    fn marker(pos: u64) -> Marker {
        Marker {
            id: 0,
            file: 1,
            pos,
            name: String::new(),
            color: 0,
            from_recorder: false,
        }
    }

    fn region(start: u64, end: u64) -> Region {
        Region {
            id: 0,
            file: 1,
            start,
            end,
            name: "riff".into(),
            color: 3,
            export: true,
            channels: ChannelMode::AsSource,
            speed: 100,
            keep_pitch: false,
            parts: Vec::new(),
        }
    }

    #[test]
    fn add_rename_undo_redo() {
        let store = MemStore::default();
        let mut markup = Markup::default();
        let mut history = History::default();

        let applied = history
            .apply(
                &mut markup,
                &store,
                vec![Change::add_region(region(10, 20))],
            )
            .unwrap();
        let Change::Region { after: Some(r), .. } = &applied[0] else {
            panic!()
        };
        assert_ne!(r.id, 0);

        let renamed = Region {
            name: "bass".into(),
            ..r.clone()
        };
        history
            .apply(
                &mut markup,
                &store,
                vec![Change::Region {
                    before: Some(r.clone()),
                    after: Some(renamed.clone()),
                }],
            )
            .unwrap();
        assert_eq!(markup.regions, std::slice::from_ref(&renamed));

        history.undo(&mut markup, &store).unwrap();
        assert_eq!(markup.regions[0].name, "riff");
        history.undo(&mut markup, &store).unwrap();
        assert!(markup.regions.is_empty() && store.regions.borrow().is_empty());
        assert!(!history.undo(&mut markup, &store).unwrap());

        history.redo(&mut markup, &store).unwrap();
        history.redo(&mut markup, &store).unwrap();
        assert_eq!(markup.regions, [renamed]);
        assert_eq!(store.regions.borrow().len(), 1);
    }

    #[test]
    fn undo_of_delete_restores_same_id() {
        let store = MemStore::default();
        let mut markup = Markup::default();
        let mut history = History::default();
        history
            .apply(
                &mut markup,
                &store,
                vec![Change::add_marker(marker(5)), Change::add_marker(marker(1))],
            )
            .unwrap();
        assert_eq!(
            markup.markers.iter().map(|m| m.pos).collect::<Vec<_>>(),
            [1, 5]
        );

        let doomed = markup.markers[1].clone();
        history
            .apply(
                &mut markup,
                &store,
                vec![Change::Marker {
                    before: Some(doomed.clone()),
                    after: None,
                }],
            )
            .unwrap();
        history.undo(&mut markup, &store).unwrap();
        assert_eq!(markup.marker(doomed.id), Some(&doomed));
    }

    #[test]
    fn failed_store_leaves_everything_untouched() {
        let store = MemStore {
            fail_regions: true,
            ..Default::default()
        };
        let mut markup = Markup::default();
        let mut history = History::default();
        let result = history.apply(
            &mut markup,
            &store,
            vec![
                Change::add_marker(marker(1)),
                Change::add_region(region(0, 5)),
            ],
        );
        assert!(result.is_err());
        assert!(markup.markers.is_empty() && store.markers.borrow().is_empty());
        assert!(!history.can_undo());
    }

    #[test]
    fn region_history_undoes_and_redoes_whole_regions() {
        let store = MemStore::default();
        let mut r = region(0, 1000);
        r.id = store.save_region(&r).unwrap();
        let mut h = RegionHistory::default();

        let mut cut = r.clone();
        cut.split_at(400);
        h.apply(&store, r.clone(), cut.clone()).unwrap();
        let mut faster = cut.clone();
        faster.speed = 200;
        h.apply(&store, cut.clone(), faster.clone()).unwrap();
        assert_eq!(store.regions.borrow()[&r.id], faster);

        assert_eq!(h.undo(&store).unwrap(), Some(cut.clone()));
        assert_eq!(h.undo(&store).unwrap(), Some(r.clone()));
        assert_eq!(store.regions.borrow()[&r.id], r);
        assert!(!h.can_undo());
        assert_eq!(h.redo(&store).unwrap(), Some(cut.clone()));
        assert_eq!(store.regions.borrow()[&r.id], cut);
        // A no-op is not an action.
        h.apply(&store, cut.clone(), cut).unwrap();
        assert!(h.can_redo());
    }
}
