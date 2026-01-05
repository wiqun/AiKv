// Multi-group Raft gRPC server adapter
// This implements the generated Raft gRPC server but dispatches requests
// to the correct openraft::Raft instance inside `MultiRaftNode` based on
// the `group_id` field in the protobuf messages.
//
// Group 0 is special: it's the MetaRaft group for cluster metadata.
// Groups 1+ are data groups managed by MultiRaftNode.

#[cfg(feature = "cluster")]
use std::sync::Arc;

#[cfg(feature = "cluster")]
use aidb::cluster::{MetaRaftNode, MultiRaftNode};

#[cfg(feature = "cluster")]
use aidb::cluster::raft_network::raft_rpc as rpc;

#[cfg(feature = "cluster")]
use aidb::cluster::raft_storage::TypeConfig;

#[cfg(feature = "cluster")]
use openraft::{LogId, LeaderId, Raft};
use openraft::raft::{AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, VoteRequest};

#[cfg(feature = "cluster")]
use tonic::{Request, Response, Status};

#[cfg(feature = "cluster")]
use rmp_serde;

/// Multi-group Raft gRPC service that handles both MetaRaft (group 0)
/// and data groups (group 1+).
#[cfg(feature = "cluster")]
#[derive(Clone)]
pub struct MultiRaftService {
    multi: Arc<MultiRaftNode>,
    meta: Option<Arc<MetaRaftNode>>,
}

#[cfg(feature = "cluster")]
impl MultiRaftService {
    pub fn new(multi: Arc<MultiRaftNode>) -> Self {
        // Get MetaRaftNode from MultiRaftNode if available
        let meta = multi.meta_raft().map(|m| m.clone());
        Self { multi, meta }
    }

    /// Get the Raft instance for a given group ID.
    /// Group 0 = MetaRaft, Group 1+ = data groups.
    fn get_raft(&self, group_id: u64) -> Option<Arc<Raft<TypeConfig>>> {
        if group_id == 0 {
            // MetaRaft group
            self.meta.as_ref().map(|m| m.raft().clone())
        } else {
            // Data group
            self.multi.get_raft_group(group_id)
        }
    }
}

#[cfg(feature = "cluster")]
#[tonic::async_trait]
impl rpc::raft_service_server::RaftService for MultiRaftService {
    async fn append_entries(
        &self,
        request: Request<rpc::AppendEntriesRequest>,
    ) -> Result<Response<rpc::AppendEntriesResponse>, Status> {
        let req = request.into_inner();

        // Lookup target Raft instance by group_id (0 = MetaRaft, 1+ = data groups)
        let group_id = req.group_id as u64;
        let raft = self
            .get_raft(group_id)
            .ok_or_else(|| Status::not_found(format!("Group {} not found", group_id)))?;

        // Convert entries
        let mut entries = Vec::new();
        for entry in req.entries {
            let payload: openraft::EntryPayload<TypeConfig> =
                rmp_serde::from_slice(&entry.payload).map_err(|e| {
                    Status::internal(format!("Failed to deserialize entry payload: {}", e))
                })?;

            entries.push(openraft::Entry {
                log_id: openraft::LogId::new(LeaderId::new(entry.log_term, entry.log_leader_id), entry.log_index),
                payload,
            });
        }

        let prev_log_id = if let (Some(index), Some(term), Some(leader_id)) = (
            req.prev_log_index,
            req.prev_log_term,
            req.prev_log_leader_id,
        ) {
            Some(LogId::new(LeaderId::new(term, leader_id), index))
        } else {
            None
        };

        let leader_commit = if let (Some(index), Some(term), Some(leader_id)) = (
            req.leader_commit_index,
            req.leader_commit_term,
            req.leader_commit_leader_id,
        ) {
            Some(LogId::new(LeaderId::new(term, leader_id), index))
        } else {
            None
        };

        let append_req = AppendEntriesRequest {
            vote: openraft::Vote {
                leader_id: LeaderId::new(req.vote_term, req.vote_node_id),
                committed: req.vote_committed,
            },
            prev_log_id,
            entries,
            leader_commit,
        };

        let append_resp = raft
            .append_entries(append_req)
            .await
            .map_err(|e| Status::internal(format!("AppendEntries failed: {}", e)))?;

        let response = match append_resp {
            AppendEntriesResponse::Success => rpc::AppendEntriesResponse {
                vote_term: 0,
                vote_node_id: 0,
                vote_committed: false,
                success: true,
                conflict_index: None,
                conflict_term: None,
            },
            AppendEntriesResponse::PartialSuccess(_) => rpc::AppendEntriesResponse {
                vote_term: 0,
                vote_node_id: 0,
                vote_committed: false,
                success: true,
                conflict_index: None,
                conflict_term: None,
            },
            AppendEntriesResponse::Conflict => rpc::AppendEntriesResponse {
                vote_term: 0,
                vote_node_id: 0,
                vote_committed: false,
                success: false,
                conflict_index: Some(0),
                conflict_term: Some(0),
            },
            AppendEntriesResponse::HigherVote(vote) => rpc::AppendEntriesResponse {
                vote_term: vote.leader_id.term,
                vote_node_id: vote.leader_id.node_id,
                vote_committed: vote.committed,
                success: false,
                conflict_index: None,
                conflict_term: None,
            },
        };

        Ok(Response::new(response))
    }

    async fn install_snapshot(
        &self,
        request: Request<rpc::InstallSnapshotRequest>,
    ) -> Result<Response<rpc::InstallSnapshotResponse>, Status> {
        let req = request.into_inner();

        // Lookup target Raft instance by group_id (0 = MetaRaft, 1+ = data groups)
        let group_id = req.group_id as u64;
        let raft = self
            .get_raft(group_id)
            .ok_or_else(|| Status::not_found(format!("Group {} not found", group_id)))?;

        let meta = req
            .meta
            .ok_or_else(|| Status::invalid_argument("Missing snapshot meta"))?;

        let last_log_id = if let (Some(index), Some(term), Some(leader_id)) = (
            meta.last_log_index,
            meta.last_log_term,
            meta.last_log_leader_id,
        ) {
            Some(LogId::new(LeaderId::new(term, leader_id), index))
        } else {
            None
        };

        let last_membership: openraft::StoredMembership<_, openraft::BasicNode> = rmp_serde::from_slice(&meta.last_membership)
            .map_err(|e| Status::internal(format!("Failed to deserialize membership: {}", e)))?;

        let snapshot_meta = openraft::SnapshotMeta { last_log_id, last_membership, snapshot_id: meta.snapshot_id };

        let install_req = InstallSnapshotRequest {
            vote: openraft::Vote {
                leader_id: LeaderId::new(req.vote_term, req.vote_node_id),
                committed: req.vote_committed,
            },
            meta: snapshot_meta,
            offset: 0,
            data: req.snapshot_data,
            done: true,
        };

        let install_resp = raft
            .install_snapshot(install_req)
            .await
            .map_err(|e| Status::internal(format!("InstallSnapshot failed: {}", e)))?;

        let response = rpc::InstallSnapshotResponse {
            vote_term: install_resp.vote.leader_id.term,
            vote_node_id: install_resp.vote.leader_id.node_id,
            vote_committed: install_resp.vote.committed,
        };

        Ok(Response::new(response))
    }

    async fn vote(
        &self,
        request: Request<rpc::VoteRequest>,
    ) -> Result<Response<rpc::VoteResponse>, Status> {
        let req = request.into_inner();

        // Lookup target Raft instance by group_id (0 = MetaRaft, 1+ = data groups)
        let group_id = req.group_id as u64;
        let raft = self
            .get_raft(group_id)
            .ok_or_else(|| Status::not_found(format!("Group {} not found", group_id)))?;

        let last_log_id = if req.last_log_index > 0 {
            Some(LogId::new(LeaderId::new(req.last_log_term, req.last_log_leader_id), req.last_log_index))
        } else {
            None
        };

        let vote_req = VoteRequest { vote: openraft::Vote { leader_id: LeaderId::new(req.vote_term, req.vote_node_id), committed: req.vote_committed }, last_log_id };

        let vote_resp = raft.vote(vote_req).await.map_err(|e| Status::internal(format!("Vote failed: {}", e)))?;

        let response = rpc::VoteResponse {
            vote_term: vote_resp.vote.leader_id.term,
            vote_node_id: vote_resp.vote.leader_id.node_id,
            vote_committed: vote_resp.vote.committed,
            vote_granted: vote_resp.vote_granted,
            is_in_membership: true,
        };

        Ok(Response::new(response))
    }
}
