use chroma_types::{
    Collection, CollectionUuid, Database, FlushCompactionResponse, GetCollectionSizeError,
    GetSegmentsError, ListDatabasesError, ListDatabasesResponse, Segment, SegmentFlushInfo,
    SegmentScope, SegmentType, Tenant,
};
use chroma_types::{GetCollectionsError, SegmentUuid};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use super::sysdb::FlushCompactionError;
use super::sysdb::GetLastCompactionTimeError;
use chroma_types::chroma_proto::VersionListForCollection;

#[derive(Clone, Debug)]
pub struct TestSysDb {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug)]
struct Inner {
    collections: HashMap<CollectionUuid, Collection>,
    segments: HashMap<SegmentUuid, Segment>,
    tenant_last_compaction_time: HashMap<String, i64>,
}

impl TestSysDb {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        TestSysDb {
            inner: Arc::new(Mutex::new(Inner {
                collections: HashMap::new(),
                segments: HashMap::new(),
                tenant_last_compaction_time: HashMap::new(),
            })),
        }
    }

    pub fn add_collection(&mut self, collection: Collection) {
        let mut inner = self.inner.lock();
        inner
            .collections
            .insert(collection.collection_id, collection);
    }

    pub fn update_collection_size(&mut self, collection_id: CollectionUuid, collection_size: u64) {
        let mut inner = self.inner.lock();
        let coll = inner
            .collections
            .get_mut(&collection_id)
            .expect("Expected collection");
        coll.total_records_post_compaction = collection_size;
    }

    pub fn add_segment(&mut self, segment: Segment) {
        let mut inner = self.inner.lock();
        inner.segments.insert(segment.id, segment);
    }

    pub fn add_tenant_last_compaction_time(&mut self, tenant: String, last_compaction_time: i64) {
        let mut inner = self.inner.lock();
        inner
            .tenant_last_compaction_time
            .insert(tenant, last_compaction_time);
    }

    fn filter_collections(
        collection: &Collection,
        collection_id: Option<CollectionUuid>,
        name: Option<String>,
        tenant: Option<String>,
        database: Option<String>,
    ) -> bool {
        if collection_id.is_some() && collection_id.unwrap() != collection.collection_id {
            return false;
        }
        if name.is_some() && name.unwrap() != collection.name {
            return false;
        }
        if tenant.is_some() && tenant.unwrap() != collection.tenant {
            return false;
        }
        if database.is_some() && database.unwrap() != collection.database {
            return false;
        }
        true
    }

    fn filter_segments(
        segment: &Segment,
        id: Option<SegmentUuid>,
        r#type: Option<String>,
        scope: Option<SegmentScope>,
        collection: CollectionUuid,
    ) -> bool {
        if id.is_some() && id.unwrap() != segment.id {
            return false;
        }
        if let Some(r#type) = r#type {
            return segment.r#type == SegmentType::try_from(r#type.as_str()).unwrap();
        }
        if scope.is_some() && scope.unwrap() != segment.scope {
            return false;
        }
        if collection != segment.collection {
            return false;
        }
        true
    }
}

impl TestSysDb {
    pub(crate) async fn get_collections(
        &mut self,
        collection_id: Option<CollectionUuid>,
        name: Option<String>,
        tenant: Option<String>,
        database: Option<String>,
    ) -> Result<Vec<Collection>, GetCollectionsError> {
        let inner = self.inner.lock();
        let mut collections = Vec::new();
        for collection in inner.collections.values() {
            if !TestSysDb::filter_collections(
                collection,
                collection_id,
                name.clone(),
                tenant.clone(),
                database.clone(),
            ) {
                continue;
            }
            collections.push(collection.clone());
        }
        Ok(collections)
    }

    pub(crate) async fn get_segments(
        &mut self,
        id: Option<SegmentUuid>,
        r#type: Option<String>,
        scope: Option<SegmentScope>,
        collection: CollectionUuid,
    ) -> Result<Vec<Segment>, GetSegmentsError> {
        let inner = self.inner.lock();
        let mut segments = Vec::new();
        for segment in inner.segments.values() {
            if !TestSysDb::filter_segments(segment, id, r#type.clone(), scope.clone(), collection) {
                continue;
            }
            segments.push(segment.clone());
        }
        Ok(segments)
    }

    pub(crate) async fn list_databases(
        &self,
        tenant: String,
        limit: Option<u32>,
        _offset: u32,
    ) -> Result<ListDatabasesResponse, ListDatabasesError> {
        let inner = self.inner.lock();
        let mut databases = Vec::new();
        let mut seen_db_names = std::collections::HashSet::new();

        for collection in inner.collections.values() {
            if collection.tenant == tenant && !seen_db_names.contains(&collection.database) {
                seen_db_names.insert(collection.database.clone());

                let db = Database {
                    id: uuid::Uuid::new_v4(),
                    name: collection.database.clone(),
                    tenant: tenant.clone(),
                };

                databases.push(db);
            }
        }

        if let Some(limit_value) = limit {
            if limit_value > 0 && databases.len() > limit_value as usize {
                databases.truncate(limit_value as usize);
            }
        }

        Ok(databases)
    }

    pub(crate) async fn get_last_compaction_time(
        &mut self,
        tenant_ids: Vec<String>,
    ) -> Result<Vec<Tenant>, GetLastCompactionTimeError> {
        let inner = self.inner.lock();
        let mut tenants = Vec::new();
        for tenant_id in tenant_ids {
            let last_compaction_time = match inner.tenant_last_compaction_time.get(&tenant_id) {
                Some(last_compaction_time) => *last_compaction_time,
                None => {
                    return Err(GetLastCompactionTimeError::TenantNotFound);
                }
            };
            tenants.push(Tenant {
                id: tenant_id,
                last_compaction_time,
            });
        }
        Ok(tenants)
    }

    pub(crate) async fn flush_compaction(
        &mut self,
        tenant_id: String,
        collection_id: CollectionUuid,
        log_position: i64,
        collection_version: i32,
        segment_flush_info: Arc<[SegmentFlushInfo]>,
        total_records_post_compaction: u64,
    ) -> Result<FlushCompactionResponse, FlushCompactionError> {
        let mut inner = self.inner.lock();
        let collection = inner.collections.get(&collection_id);
        if collection.is_none() {
            return Err(FlushCompactionError::CollectionNotFound);
        }
        let collection = collection.unwrap();
        let mut collection = collection.clone();
        collection.log_position = log_position;
        let new_collection_version = collection_version + 1;
        collection.version = new_collection_version;
        collection.total_records_post_compaction = total_records_post_compaction;
        inner
            .collections
            .insert(collection.collection_id, collection);
        let mut last_compaction_time = match inner.tenant_last_compaction_time.get(&tenant_id) {
            Some(last_compaction_time) => *last_compaction_time,
            None => 0,
        };
        last_compaction_time += 1;

        // update segments
        for segment_flush_info in segment_flush_info.iter() {
            let segment = inner.segments.get(&segment_flush_info.segment_id);
            if segment.is_none() {
                return Err(FlushCompactionError::SegmentNotFound);
            }
            let mut segment = segment.unwrap().clone();
            segment.file_path = segment_flush_info.file_paths.clone();
            inner.segments.insert(segment.id, segment);
        }

        Ok(FlushCompactionResponse::new(
            collection_id,
            new_collection_version,
            last_compaction_time,
        ))
    }

    pub(crate) async fn mark_version_for_deletion(
        &self,
        _epoch_id: i64,
        versions: Vec<VersionListForCollection>,
    ) -> Result<(), String> {
        // For testing success case, return Ok when versions are not empty
        if !versions.is_empty() && !versions[0].versions.is_empty() {
            // Simulate error case when version is 1
            if versions[0].versions.contains(&1) {
                return Err("Failed to mark version for deletion".to_string());
            }
            Ok(())
        } else {
            Ok(())
        }
    }

    pub async fn delete_collection_version(
        &self,
        _versions: Vec<VersionListForCollection>,
    ) -> HashMap<String, bool> {
        // For testing, return success for all collections
        let mut results = HashMap::new();
        for version_list in _versions {
            results.insert(version_list.collection_id, true);
        }
        results
    }

    pub(crate) async fn get_collection_size(
        &self,
        collection_id: CollectionUuid,
    ) -> Result<usize, GetCollectionSizeError> {
        let inner = self.inner.lock();
        let collection = inner.collections.get(&collection_id);
        match collection {
            Some(collection) => Ok(collection.total_records_post_compaction as usize),
            None => Err(GetCollectionSizeError::NotFound(
                "Collection not found".to_string(),
            )),
        }
    }
}
