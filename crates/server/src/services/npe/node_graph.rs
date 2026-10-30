//! Node graph messages from nodegraph.client.graph have no running server graph.
//! No database is used; tutorial graph execution is unsupported.

use super::super::{Call, CallResult};
use crate::client::Client;
use marshal::Value;
use tracing::debug;

pub(super) fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        // Client graphs talk to the server graph that started them; none run here, so
        // there is no server graph instance to deliver to (nodegraph.client.graph).
        ("node_graph", "process_client_graph_message") | ("node_graph", "process_client_blackboard_request") => {
            debug!(user = c.login.user_id, "node_graph::{} with no server graph: {:?}", call.method, call.args);
            Ok(Value::None)
        }
        ("node_graph", "qa_subscribe") => Ok(Value::list([])),
        ("node_graph", m) if m.starts_with("qa_") => Ok(Value::None),
        _ => return None,
    })
}
