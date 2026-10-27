use thruput_open_rpc::{Project, ProjectInfo};
use thruput_open_rpc_macros::open_rpc;

// The macro consumes this declaration. Its request is documented directly,
// without generating a jsonrpsee client or server wrapper.
#[open_rpc]
trait NativeApi {
    /// List the entries.
    #[method(name = "getEntries")]
    async fn get_entries(
        &self,
        request: std::collections::BTreeMap<String, String>,
    ) -> jsonrpsee::core::RpcResult<String>;
}

#[test]
fn module_doc_emits_a_bare_method_with_a_whole_object_schema() {
    let mut project = Project::new(ProjectInfo {
        title: "Native".to_owned(),
        version: "0.1.0".to_owned(),
    });
    project.module(NativeApiOpenRpc::module_doc);
    let json = project.to_json();
    let method = &json["methods"][0];
    assert_eq!(method["name"], "getEntries");
    assert_eq!(method["summary"], "List the entries.");
    assert_eq!(method["paramStructure"], "by-name");
    assert_eq!(method["x-params-schema"]["type"], "object");
    assert!(
        method["x-params-schema"]["properties"]
            .get("request")
            .is_none()
    );
    assert_eq!(method["result"]["schema"]["type"], "string");
}
