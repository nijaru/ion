//! A layout candidate is inert until the physical publisher acknowledges it.
use std::rc::Rc;

/// Episode identity changes on edits, selection, focus and geometry invalidation.
pub(crate) struct Visibility<C> {
    episode: Rc<()>,
    painted: Option<C>,
}

pub(crate) struct Prepared<C> {
    episode: Rc<()>,
    candidate: C,
}

impl<C> Default for Visibility<C> {
    fn default() -> Self {
        Self {
            episode: Rc::new(()),
            painted: None,
        }
    }
}

impl<C> Visibility<C> {
    pub(crate) fn invalidate(&mut self) {
        self.episode = Rc::new(());
        self.painted = None;
    }

    pub(crate) fn prepare(&self, candidate: C) -> Prepared<C> {
        Prepared {
            episode: self.episode.clone(),
            candidate,
        }
    }

    /// Called only after successful publication of the exact prepared rows.
    pub(crate) fn observe(&mut self, prepared: Prepared<C>) {
        // A stale successful write can cover a newer frame. It cannot leave
        // that newer candidate authorized underneath the overwritten cells.
        self.painted = None;
        if Rc::ptr_eq(&self.episode, &prepared.episode) {
            self.painted = Some(prepared.candidate);
        }
    }

    pub(crate) fn painted(&self) -> Option<&C> {
        self.painted.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparation_and_stale_publication_grant_no_authority() {
        let mut visibility = Visibility::<&str>::default();
        let rejected = visibility.prepare("captured choice");
        assert!(visibility.painted().is_none());
        drop(rejected); // Geometry rejection or failed write.
        assert!(visibility.painted().is_none());
        let stale = visibility.prepare("captured choice");
        visibility.invalidate();
        visibility.observe(stale);
        assert!(visibility.painted().is_none());
        let overwritten = visibility.prepare("old choice");
        visibility.invalidate();
        let exact = visibility.prepare("captured choice");
        visibility.observe(exact);
        assert_eq!(visibility.painted(), Some(&"captured choice"));
        visibility.observe(overwritten);
        assert!(visibility.painted().is_none());
        visibility.invalidate();
        assert!(visibility.painted().is_none());
    }
}
