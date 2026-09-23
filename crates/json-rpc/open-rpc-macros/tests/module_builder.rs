use gh_open_rpc::{Project, ProjectInfo};
use groundhog_open_rpc_macros::open_rpc;
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;

mod docs_alias {
    pub use gh_open_rpc::{Project, ProjectInfo};
}

#[open_rpc(namespace = "demo", tag = "Demo")]
#[rpc(client, server, namespace = "demo")]
trait DemoApi {
    /// Echo the input back.
    #[method(name = "echo")]
    async fn echo(&self, value: String) -> RpcResult<String>;
}

#[test]
fn generated_module_doc_appends_to_project_builder() {
    let mut project = Project::new(ProjectInfo {
        title: "Demo".to_owned(),
        version: "0.1.0".to_owned(),
    });
    project.module(DemoApiOpenRpc::module_doc);

    let json = project.to_json();
    assert_eq!(json["methods"][0]["name"], "demo_echo");
    assert_eq!(json["methods"][0]["params"][0]["schema"]["type"], "string");
    assert_eq!(json["methods"][0]["result"]["schema"]["type"], "string");
}

#[open_rpc(namespace = "alias", tag = "Alias")]
#[rpc(client, server, namespace = "alias")]
trait AliasApi {
    #[method(name = "ping")]
    async fn ping(&self) -> RpcResult<u64>;
}

#[test]
fn generated_module_doc_compiles_with_reexported_project_imports() {
    let mut project = docs_alias::Project::new(docs_alias::ProjectInfo {
        title: "Alias".to_owned(),
        version: "0.1.0".to_owned(),
    });
    project.module(AliasApiOpenRpc::module_doc);

    let json = project.to_json();
    assert_eq!(json["methods"][0]["name"], "alias_ping");
    assert_eq!(json["methods"][0]["result"]["schema"]["type"], "integer");
}

#[test]
fn generated_param_names_list_each_method_in_declaration_order() {
    assert_eq!(
        DemoApiOpenRpc::PARAM_NAMES,
        &[("demo_echo", &["value"][..])]
    );
    assert_eq!(AliasApiOpenRpc::PARAM_NAMES, &[("alias_ping", &[][..])]);
}
