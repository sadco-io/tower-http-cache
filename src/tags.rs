//! Cache tags and invalidation groups.
//!
//! This module provides a tag-based invalidation system that allows multiple
//! cache entries to be invalidated together by shared tags. Tags enable
//! efficient bulk invalidation patterns like "all posts by user:123" or
//! "all tenant:acme entries".

use dashmap::DashMap;
use std::collections::HashSet;
use std::sync::Arc;

/// Thread-safe index mapping tags to cache keys.
///
/// The TagIndex supports efficient lookup of all cache keys associated
/// with a particular tag, enabling bulk invalidation operations.
#[derive(Clone)]
pub struct TagIndex {
    /// Maps tag → set of cache keys
    forward: Arc<DashMap<String, HashSet<String>>>,
    /// Maps cache key → set of tags
    reverse: Arc<DashMap<String, HashSet<String>>>,
}

impl TagIndex {
    /// Creates a new empty tag index.
    pub fn new() -> Self {
        Self {
            forward: Arc::new(DashMap::new()),
            reverse: Arc::new(DashMap::new()),
        }
    }

    /// Associates a cache key with multiple tags.
    ///
    /// This creates bidirectional mappings between the key and all tags,
    /// allowing efficient lookup in either direction.
    pub fn index(&self, key: String, tags: Vec<String>) {
        if tags.is_empty() {
            return;
        }

        // Update forward index (tag → keys)
        for tag in &tags {
            self.forward
                .entry(tag.clone())
                .or_insert_with(HashSet::new)
                .insert(key.clone());
        }

        // Update reverse index (key → tags)
        self.reverse
            .entry(key)
            .or_insert_with(HashSet::new)
            .extend(tags);
    }

    /// Retrieves all cache keys associated with a tag.
    pub fn get_keys_by_tag(&self, tag: &str) -> Vec<String> {
        self.forward
            .get(tag)
            .map(|keys| keys.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Retrieves all tags associated with a cache key.
    pub fn get_tags_by_key(&self, key: &str) -> Vec<String> {
        self.reverse
            .get(key)
            .map(|tags| tags.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Removes a cache key and all its tag associations.
    ///
    /// This cleans up both the forward and reverse indexes, removing
    /// orphaned tag entries when no more keys are associated with a tag.
    pub fn remove(&self, key: &str) {
        // Get tags for this key
        if let Some((_, tags)) = self.reverse.remove(key) {
            // Remove key from all tag entries
            for tag in tags {
                if let Some(mut keys) = self.forward.get_mut(&tag) {
                    keys.remove(key);
                    // Clean up empty tag entries
                    if keys.is_empty() {
                        drop(keys);
                        self.forward.remove(&tag);
                    }
                }
            }
        }
    }

    /// Removes all keys associated with a tag.
    ///
    /// Returns the list of keys that were removed.
    pub fn remove_by_tag(&self, tag: &str) -> Vec<String> {
        if let Some((_, keys)) = self.forward.remove(tag) {
            let keys_vec: Vec<String> = keys.iter().cloned().collect();

            // Clean up reverse index
            for key in &keys_vec {
                if let Some(mut tags) = self.reverse.get_mut(key) {
                    tags.remove(tag);
                    if tags.is_empty() {
                        drop(tags);
                        self.reverse.remove(key);
                    }
                }
            }

            keys_vec
        } else {
            Vec::new()
        }
    }

    /// Removes all keys associated with multiple tags.
    ///
    /// Returns the deduplicated list of all keys that were removed.
    pub fn remove_by_tags(&self, tags: &[String]) -> Vec<String> {
        let mut all_keys = HashSet::new();

        for tag in tags {
            let keys = self.remove_by_tag(tag);
            all_keys.extend(keys);
        }

        all_keys.into_iter().collect()
    }

    /// Returns all currently indexed tags.
    pub fn list_tags(&self) -> Vec<String> {
        self.forward
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Returns the number of unique tags in the index.
    pub fn tag_count(&self) -> usize {
        self.forward.len()
    }

    /// Returns the number of unique keys in the index.
    pub fn key_count(&self) -> usize {
        self.reverse.len()
    }

    /// Clears all tag associations.
    pub fn clear(&self) {
        self.forward.clear();
        self.reverse.clear();
    }
}

impl Default for TagIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration for tag-based caching.
#[derive(Debug, Clone)]
pub struct TagPolicy {
    /// Enable tag support
    pub enabled: bool,

    /// Maximum number of tags per entry
    pub max_tags_per_entry: usize,
}

impl Default for TagPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            max_tags_per_entry: 10,
        }
    }
}

impl TagPolicy {
    /// Creates a new tag policy with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enables tag support.
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Sets the maximum number of tags per entry.
    pub fn with_max_tags_per_entry(mut self, max: usize) -> Self {
        self.max_tags_per_entry = max;
        self
    }

    /// Validates and truncates tags according to policy.
    pub fn validate_tags(&self, tags: Vec<String>) -> Vec<String> {
        if !self.enabled {
            return Vec::new();
        }

        tags.into_iter()
            .filter(|t| !t.is_empty())
            .take(self.max_tags_per_entry)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_index_new_is_empty() {
        let index = TagIndex::new();
        assert_eq!(index.tag_count(), 0);
        assert_eq!(index.key_count(), 0);
    }

    #[test]
    fn tag_index_indexes_keys_with_tags() {
        let index = TagIndex::new();
        index.index(
            "key1".to_string(),
            vec!["user:123".to_string(), "posts".to_string()],
        );

        assert_eq!(index.get_keys_by_tag("user:123"), vec!["key1"]);
        assert_eq!(index.get_keys_by_tag("posts"), vec!["key1"]);
        assert_eq!(index.tag_count(), 2);
        assert_eq!(index.key_count(), 1);
    }

    #[test]
    fn tag_index_multiple_keys_same_tag() {
        let index = TagIndex::new();
        index.index("key1".to_string(), vec!["user:123".to_string()]);
        index.index("key2".to_string(), vec!["user:123".to_string()]);

        let keys = index.get_keys_by_tag("user:123");
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&"key1".to_string()));
        assert!(keys.contains(&"key2".to_string()));
    }

    #[test]
    fn tag_index_get_tags_by_key() {
        let index = TagIndex::new();
        index.index(
            "key1".to_string(),
            vec!["tag1".to_string(), "tag2".to_string()],
        );

        let tags = index.get_tags_by_key("key1");
        assert_eq!(tags.len(), 2);
        assert!(tags.contains(&"tag1".to_string()));
        assert!(tags.contains(&"tag2".to_string()));
    }

    #[test]
    fn tag_index_remove_cleans_up() {
        let index = TagIndex::new();
        index.index(
            "key1".to_string(),
            vec!["tag1".to_string(), "tag2".to_string()],
        );

        index.remove("key1");

        assert_eq!(index.get_keys_by_tag("tag1").len(), 0);
        assert_eq!(index.get_keys_by_tag("tag2").len(), 0);
        assert_eq!(index.tag_count(), 0);
        assert_eq!(index.key_count(), 0);
    }

    #[test]
    fn tag_index_remove_preserves_other_keys() {
        let index = TagIndex::new();
        index.index("key1".to_string(), vec!["shared".to_string()]);
        index.index("key2".to_string(), vec!["shared".to_string()]);

        index.remove("key1");

        assert_eq!(index.get_keys_by_tag("shared"), vec!["key2"]);
        assert_eq!(index.tag_count(), 1);
        assert_eq!(index.key_count(), 1);
    }

    #[test]
    fn tag_index_remove_by_tag() {
        let index = TagIndex::new();
        index.index("key1".to_string(), vec!["user:123".to_string()]);
        index.index("key2".to_string(), vec!["user:123".to_string()]);

        let removed = index.remove_by_tag("user:123");

        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&"key1".to_string()));
        assert!(removed.contains(&"key2".to_string()));
        assert_eq!(index.tag_count(), 0);
        assert_eq!(index.key_count(), 0);
    }

    #[test]
    fn tag_index_remove_by_tag_cleans_reverse_index() {
        let index = TagIndex::new();
        index.index(
            "key1".to_string(),
            vec!["tag1".to_string(), "tag2".to_string()],
        );

        index.remove_by_tag("tag1");

        // key1 should still have tag2
        let tags = index.get_tags_by_key("key1");
        assert_eq!(tags, vec!["tag2"]);
    }

    #[test]
    fn tag_index_remove_by_tags() {
        let index = TagIndex::new();
        index.index("key1".to_string(), vec!["tag1".to_string()]);
        index.index("key2".to_string(), vec!["tag2".to_string()]);
        index.index("key3".to_string(), vec!["tag3".to_string()]);

        let removed = index.remove_by_tags(&["tag1".to_string(), "tag2".to_string()]);

        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&"key1".to_string()));
        assert!(removed.contains(&"key2".to_string()));
        assert!(!removed.contains(&"key3".to_string()));
    }

    #[test]
    fn tag_index_list_tags() {
        let index = TagIndex::new();
        index.index("key1".to_string(), vec!["tag1".to_string()]);
        index.index("key2".to_string(), vec!["tag2".to_string()]);

        let tags = index.list_tags();
        assert_eq!(tags.len(), 2);
        assert!(tags.contains(&"tag1".to_string()));
        assert!(tags.contains(&"tag2".to_string()));
    }

    #[test]
    fn tag_index_clear() {
        let index = TagIndex::new();
        index.index("key1".to_string(), vec!["tag1".to_string()]);

        index.clear();

        assert_eq!(index.tag_count(), 0);
        assert_eq!(index.key_count(), 0);
    }

    #[test]
    fn tag_policy_default() {
        let policy = TagPolicy::default();
        assert!(!policy.enabled);
        assert_eq!(policy.max_tags_per_entry, 10);
    }

    #[test]
    fn tag_policy_builder() {
        let policy = TagPolicy::new()
            .with_enabled(true)
            .with_max_tags_per_entry(5);

        assert!(policy.enabled);
        assert_eq!(policy.max_tags_per_entry, 5);
    }

    #[test]
    fn tag_policy_validate_tags_when_disabled() {
        let policy = TagPolicy::new().with_enabled(false);
        let tags = vec!["tag1".to_string(), "tag2".to_string()];
        let validated = policy.validate_tags(tags);
        assert_eq!(validated.len(), 0);
    }

    #[test]
    fn tag_policy_validate_tags_filters_empty() {
        let policy = TagPolicy::new().with_enabled(true);
        let tags = vec!["tag1".to_string(), "".to_string(), "tag2".to_string()];
        let validated = policy.validate_tags(tags);
        assert_eq!(validated.len(), 2);
        assert!(validated.contains(&"tag1".to_string()));
        assert!(validated.contains(&"tag2".to_string()));
    }

    #[test]
    fn tag_policy_validate_tags_truncates() {
        let policy = TagPolicy::new()
            .with_enabled(true)
            .with_max_tags_per_entry(2);
        let tags = vec!["tag1".to_string(), "tag2".to_string(), "tag3".to_string()];
        let validated = policy.validate_tags(tags);
        assert_eq!(validated.len(), 2);
    }

    #[test]
    fn tag_index_concurrent_access() {
        use std::thread;

        let index = Arc::new(TagIndex::new());
        let mut handles = vec![];

        for i in 0..10 {
            let index = index.clone();
            let handle = thread::spawn(move || {
                let key = format!("key{}", i);
                let tag = format!("tag{}", i % 3);
                index.index(key.clone(), vec![tag.clone()]);
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert!(index.key_count() <= 10);
        assert!(index.tag_count() <= 3);
    }
}
