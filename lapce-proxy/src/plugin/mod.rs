pub mod catalog;
pub mod dap;
pub mod lsp;
pub mod psp;
pub mod wasi;

use std::{
    borrow::Cow,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, anyhow};
use crossbeam_channel::{Receiver, Sender};
use dyn_clone::DynClone;
use flate2::read::GzDecoder;
use jsonrpc_lite::Id;
use lapce_rpc::{
    RequestId, RpcError,
    core::CoreRpcHandler,
    dap_types::{self, DapId, RunDebugConfig, SourceBreakpoint, ThreadId},
    plugin::{PluginId, VoltInfo, VoltMetadata},
    proxy::ProxyRpcHandler,
    style::LineStyle,
    terminal::TermId,
};
use lapce_xi_rope::{Rope, RopeDelta};
use log::error;
use lsp_types::{
    CallHierarchyClientCapabilities, CallHierarchyIncomingCall,
    CallHierarchyIncomingCallsParams, CallHierarchyItem, CallHierarchyPrepareParams,
    ClientCapabilities, CodeAction, CodeActionCapabilityResolveSupport,
    CodeActionClientCapabilities, CodeActionContext, CodeActionKind,
    CodeActionKindLiteralSupport, CodeActionLiteralSupport, CodeActionParams,
    CodeActionResponse, CodeLens, CodeLensParams, CompletionClientCapabilities,
    CompletionItem, CompletionItemCapability,
    CompletionItemCapabilityResolveSupport, CompletionParams, CompletionResponse,
    Diagnostic, DocumentFormattingParams, DocumentHighlight,
    DocumentHighlightClientCapabilities, DocumentHighlightParams,
    DocumentSymbolClientCapabilities, DocumentSymbolParams, DocumentSymbolResponse,
    FoldingRange, FoldingRangeClientCapabilities, FoldingRangeParams,
    FormattingOptions, GotoCapability, GotoDefinitionParams, GotoDefinitionResponse,
    Hover, HoverClientCapabilities, HoverParams, InlayHint,
    InlayHintClientCapabilities, InlayHintParams,
    InlineCompletionClientCapabilities, InlineCompletionParams,
    InlineCompletionResponse, InlineCompletionTriggerKind, Location, MarkupKind,
    MessageActionItemCapabilities, ParameterInformationSettings,
    PartialResultParams, Position, PrepareRenameResponse,
    PublishDiagnosticsClientCapabilities, Range, ReferenceContext, ReferenceParams,
    RenameParams, SelectionRange, SelectionRangeParams, SemanticTokens,
    SemanticTokensClientCapabilities, SemanticTokensClientCapabilitiesRequests,
    SemanticTokensDeltaParams, SemanticTokensFullDeltaResult,
    SemanticTokensFullOptions, SemanticTokensParams,
    ShowMessageRequestClientCapabilities, SignatureHelp,
    SignatureHelpClientCapabilities, SignatureHelpParams,
    SignatureInformationSettings, SymbolInformation, TextDocumentClientCapabilities,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams,
    TextDocumentSyncClientCapabilities, TextEdit, Url,
    VersionedTextDocumentIdentifier, WindowClientCapabilities,
    WorkDoneProgressParams, WorkspaceClientCapabilities, WorkspaceEdit,
    WorkspaceSymbolClientCapabilities, WorkspaceSymbolParams,
    request::{
        CallHierarchyIncomingCalls, CallHierarchyPrepare, CodeActionRequest,
        CodeActionResolveRequest, CodeLensRequest, CodeLensResolve, Completion,
        DocumentHighlightRequest, DocumentSymbolRequest, FoldingRangeRequest,
        Formatting, GotoDefinition, GotoImplementation, GotoImplementationResponse,
        GotoTypeDefinition, GotoTypeDefinitionParams, GotoTypeDefinitionResponse,
        HoverRequest, InlayHintRequest, InlineCompletionRequest,
        PrepareRenameRequest, References, Rename, Request, ResolveCompletionItem,
        SelectionRangeRequest, SemanticTokensFullDeltaRequest,
        SemanticTokensFullRequest, SignatureHelpRequest, WorkspaceSymbolRequest,
    },
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use tar::Archive;

use self::{
    catalog::PluginCatalog,
    dap::DapRpcHandler,
    psp::{ClonableCallback, PluginServerRpcHandler, RpcCallback},
    wasi::{load_volt, start_volt},
};
use crate::buffer::language_id_from_path;

pub type PluginName = String;

#[allow(clippy::large_enum_variant)]
pub enum PluginCatalogRpc {
    ServerRequest {
        plugin_id:    Option<PluginId>,
        request_sent: Option<Arc<AtomicUsize>>,
        method:       Cow<'static, str>,
        params:       Value,
        language_id:  Option<String>,
        path:         Option<PathBuf>,
        check:        bool,
        id:           u64,
        f:            Box<dyn ClonableCallback<Value, RpcError>>,
    },
    ServerNotification {
        plugin_id:   Option<PluginId>,
        method:      Cow<'static, str>,
        params:      Value,
        language_id: Option<String>,
        path:        Option<PathBuf>,
        check:       bool,
    },
    FormatSemanticTokens {
        id:        u64,
        plugin_id: PluginId,
        tokens:    SemanticTokens,
        text:      Rope,
        f:         Box<dyn RpcCallback<(Vec<LineStyle>, Option<String>), RpcError>>,
    },
    DapVariable {
        dap_id:    DapId,
        reference: usize,
        f:         Box<dyn RpcCallback<Vec<dap_types::Variable>, RpcError>>,
    },
    DapGetScopes {
        dap_id:   DapId,
        frame_id: usize,
        f: Box<
            dyn RpcCallback<
                    Vec<(dap_types::Scope, Vec<dap_types::Variable>)>,
                    RpcError,
                >,
        >,
    },
    DidOpenTextDocument {
        document: TextDocumentItem,
        id:       u64,
    },
    DidChangeTextDocument {
        language_id: String,
        document:    VersionedTextDocumentIdentifier,
        delta:       RopeDelta,
        text:        Rope,
        new_text:    Rope,
    },
    DidSaveTextDocument {
        language_id:   String,
        path:          PathBuf,
        text_document: TextDocumentIdentifier,
        text:          Rope,
    },
    Handler(PluginCatalogNotification),
    RemoveVolt {
        volt: VoltInfo,
        f:    Box<dyn ClonableCallback<Value, RpcError>>,
        id:   u64,
    },
    Shutdown,
}

#[allow(clippy::large_enum_variant)]
pub enum PluginCatalogNotification {
    UpdatePluginConfigs(HashMap<String, HashMap<String, serde_json::Value>>),
    UnactivatedVolts(Vec<VoltMetadata>, u64),
    PluginServerLoaded(PluginServerRpcHandler),
    InstallVolt(VoltInfo, u64),
    StopVolt(VoltInfo),
    EnableVolt(VoltInfo, u64),
    ReloadVolt(VoltMetadata, u64),
    DapLoaded(DapRpcHandler),
    DapDisconnected(DapId),
    DapStart {
        config:      RunDebugConfig,
        breakpoints: HashMap<PathBuf, Vec<SourceBreakpoint>>,
    },
    DapProcessId {
        dap_id:     DapId,
        process_id: Option<u32>,
        term_id:    TermId,
    },
    DapContinue {
        dap_id:    DapId,
        thread_id: ThreadId,
    },
    DapStepOver {
        dap_id:    DapId,
        thread_id: ThreadId,
    },
    DapStepInto {
        dap_id:    DapId,
        thread_id: ThreadId,
    },
    DapStepOut {
        dap_id:    DapId,
        thread_id: ThreadId,
    },
    DapPause {
        dap_id:    DapId,
        thread_id: ThreadId,
    },
    DapStop {
        dap_id: DapId,
    },
    DapDisconnect {
        dap_id: DapId,
    },
    DapRestart {
        config:      RunDebugConfig,
        breakpoints: HashMap<PathBuf, Vec<SourceBreakpoint>>,
    },
    DapSetBreakpoints {
        dap_id:      DapId,
        path:        PathBuf,
        breakpoints: Vec<SourceBreakpoint>,
    },
    RegisterDebuggerType {
        debugger_type: String,
        program:       String,
        args:          Option<Vec<String>>,
    },
    Shutdown,
}

#[derive(Clone)]
pub struct PluginCatalogRpcHandler {
    core_rpc:  CoreRpcHandler,
    proxy_rpc: ProxyRpcHandler,
    plugin_tx: Sender<PluginCatalogRpc>,
    plugin_rx: Arc<Mutex<Option<Receiver<PluginCatalogRpc>>>>,
    #[allow(dead_code, clippy::type_complexity)]
    pending:   Arc<Mutex<HashMap<u64, Sender<Result<Value, RpcError>>>>>,
}

impl PluginCatalogRpcHandler {
    pub fn new(core_rpc: CoreRpcHandler, proxy_rpc: ProxyRpcHandler) -> Self {
        let (plugin_tx, plugin_rx) = crossbeam_channel::unbounded();
        Self {
            core_rpc,
            proxy_rpc,
            plugin_tx,
            plugin_rx: Arc::new(Mutex::new(Some(plugin_rx))),
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[allow(dead_code)]
    fn handle_response(&self, id: RequestId, result: Result<Value, RpcError>) {
        if let Some(chan) = { self.pending.lock().remove(&id) } {
            if let Err(err) = chan.send(result) {
                log::error!("{:?}", err);
            }
        }
    }

    #[tokio::main]
    pub async fn mainloop(&self, plugin: &mut PluginCatalog) {
        let plugin_rx = self.plugin_rx.lock().take().unwrap();
        for msg in plugin_rx {
            match msg {
                PluginCatalogRpc::ServerRequest {
                    plugin_id,
                    request_sent,
                    method,
                    params,
                    language_id,
                    path,
                    check,
                    id,
                    f,
                } => {
                    plugin.handle_server_request(
                        plugin_id,
                        request_sent,
                        method,
                        params,
                        language_id,
                        path,
                        check,
                        id,
                        f,
                    );
                },
                PluginCatalogRpc::ServerNotification {
                    plugin_id,
                    method,
                    params,
                    language_id,
                    path,
                    check,
                } => {
                    plugin.handle_server_notification(
                        plugin_id,
                        method,
                        params,
                        language_id,
                        path,
                        check,
                    );
                },
                PluginCatalogRpc::Handler(notification) => {
                    plugin.handle_notification(notification).await;
                },
                PluginCatalogRpc::FormatSemanticTokens {
                    plugin_id,
                    tokens,
                    text,
                    f,
                    id,
                } => {
                    plugin.format_semantic_tokens(id, plugin_id, tokens, text, f);
                },
                PluginCatalogRpc::DidOpenTextDocument { document, id } => {
                    plugin.handle_did_open_text_document(document, id);
                },
                PluginCatalogRpc::DidSaveTextDocument {
                    language_id,
                    path,
                    text_document,
                    text,
                } => {
                    plugin.handle_did_save_text_document(
                        language_id,
                        path,
                        text_document,
                        text,
                    );
                },
                PluginCatalogRpc::DidChangeTextDocument {
                    language_id,
                    document,
                    delta,
                    text,
                    new_text,
                } => {
                    plugin.handle_did_change_text_document(
                        language_id,
                        document,
                        delta,
                        text,
                        new_text,
                    );
                },
                PluginCatalogRpc::DapVariable {
                    dap_id,
                    reference,
                    f,
                } => {
                    plugin.dap_variable(dap_id, reference, f);
                },
                PluginCatalogRpc::DapGetScopes {
                    dap_id,
                    frame_id,
                    f,
                } => {
                    plugin.dap_get_scopes(dap_id, frame_id, f);
                },
                PluginCatalogRpc::Shutdown => {
                    return;
                },
                PluginCatalogRpc::RemoveVolt { volt, f, id } => {
                    plugin.shutdown_volt(volt, f, id);
                },
            }
        }
    }

    pub fn shutdown(&self) {
        if let Err(err) =
            self.catalog_notification(PluginCatalogNotification::Shutdown)
        {
            log::error!("{:?}", err);
        }
        if let Err(err) = self.plugin_tx.send(PluginCatalogRpc::Shutdown) {
            log::error!("{:?}", err);
        }
    }

    fn catalog_notification(
        &self,
        notification: PluginCatalogNotification,
    ) -> Result<()> {
        self.plugin_tx
            .send(PluginCatalogRpc::Handler(notification))
            .map_err(|e| anyhow!(e.to_string()))?;
        Ok(())
    }

    fn send_request_to_all_plugins<P, Resp>(
        &self,
        method: &'static str,
        params: P,
        language_id: Option<String>,
        path: Option<PathBuf>,
        id: u64,
        cb: impl FnOnce(PluginId, Result<Resp, RpcError>) + Clone + Send + 'static,
    ) where
        P: Serialize,
        Resp: DeserializeOwned, {
        let got_success = Arc::new(AtomicBool::new(false));
        let request_sent = Arc::new(AtomicUsize::new(0));
        let err_received = Arc::new(AtomicUsize::new(0));
        self.send_request(
            None,
            Some(request_sent.clone()),
            method,
            params,
            language_id,
            path,
            true,
            id,
            move |_, plugin_id, result| {
                if got_success.load(Ordering::Acquire) {
                    return;
                }
                let result = match result {
                    Ok(value) => {
                        if let Ok(item) = serde_json::from_value::<Resp>(value) {
                            got_success.store(true, Ordering::Release);
                            Ok(item)
                        } else {
                            Err(RpcError {
                                code:    0,
                                message: "deserialize error".to_string(),
                            })
                        }
                    },
                    Err(e) => Err(e),
                };
                if result.is_ok() {
                    cb(plugin_id, result)
                } else {
                    let rx = err_received.fetch_add(1, Ordering::Relaxed) + 1;
                    if request_sent.load(Ordering::Acquire) == rx {
                        cb(plugin_id, result)
                    }
                }
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn send_request<P: Serialize>(
        &self,
        plugin_id: Option<PluginId>,
        request_sent: Option<Arc<AtomicUsize>>,
        method: impl Into<Cow<'static, str>>,
        params: P,
        language_id: Option<String>,
        path: Option<PathBuf>,
        check: bool,
        id: u64,
        f: impl FnOnce(Id, PluginId, Result<Value, RpcError>) + Send + DynClone + 'static,
    ) {
        let params = serde_json::to_value(params).unwrap();
        let rpc = PluginCatalogRpc::ServerRequest {
            plugin_id,
            request_sent,
            method: method.into(),
            params,
            language_id,
            path,
            check,
            id,
            f: Box::new(f),
        };
        if let Err(err) = self.plugin_tx.send(rpc) {
            log::error!("{:?}", err);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn send_notification<P: Serialize>(
        &self,
        plugin_id: Option<PluginId>,
        method: impl Into<Cow<'static, str>>,
        params: P,
        language_id: Option<String>,
        path: Option<PathBuf>,
        check: bool,
    ) {
        let params = serde_json::to_value(params).unwrap();
        let rpc = PluginCatalogRpc::ServerNotification {
            plugin_id,
            method: method.into(),
            params,
            language_id,
            path,
            check,
        };
        if let Err(err) = self.plugin_tx.send(rpc) {
            log::error!("{:?}", err);
        }
    }

    pub fn format_semantic_tokens(
        &self,
        id: u64,
        plugin_id: PluginId,
        tokens: SemanticTokens,
        text: Rope,
        f: Box<dyn RpcCallback<(Vec<LineStyle>, Option<String>), RpcError>>,
    ) {
        if let Err(err) =
            self.plugin_tx.send(PluginCatalogRpc::FormatSemanticTokens {
                id,
                plugin_id,
                tokens,
                text,
                f,
            })
        {
            log::error!("{:?}", err);
        }
    }

    pub fn did_save_text_document(&self, path: &Path, text: Rope) {
        let text_document =
            TextDocumentIdentifier::new(Url::from_file_path(path).unwrap());
        let language_id = language_id_from_path(path).unwrap_or("").to_string();
        if let Err(err) =
            self.plugin_tx.send(PluginCatalogRpc::DidSaveTextDocument {
                language_id,
                text_document,
                path: path.into(),
                text,
            })
        {
            log::error!("{:?}", err);
        }
    }

    pub fn did_change_text_document(
        &self,
        path: &Path,
        rev: u64,
        delta: RopeDelta,
        text: Rope,
        new_text: Rope,
    ) {
        let document = VersionedTextDocumentIdentifier::new(
            Url::from_file_path(path).unwrap(),
            rev as i32,
        );
        let language_id = language_id_from_path(path).unwrap_or("").to_string();
        if let Err(err) =
            self.plugin_tx
                .send(PluginCatalogRpc::DidChangeTextDocument {
                    language_id,
                    document,
                    delta,
                    text,
                    new_text,
                })
        {
            log::error!("{:?}", err);
        }
    }

    pub fn get_definition(
        &self,
        path: &Path,
        position: Position,
        cb: impl FnOnce(PluginId, Result<GotoDefinitionResponse, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = GotoDefinition::METHOD;
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params:     WorkDoneProgressParams::default(),
            partial_result_params:         PartialResultParams::default(),
        };

        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn get_type_definition(
        &self,
        path: &Path,
        position: Position,
        cb: impl FnOnce(PluginId, Result<GotoTypeDefinitionResponse, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = GotoTypeDefinition::METHOD;
        let params = GotoTypeDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params:     WorkDoneProgressParams::default(),
            partial_result_params:         PartialResultParams::default(),
        };

        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn call_hierarchy_incoming(
        &self,
        path: &Path,
        item: CallHierarchyItem,
        cb: impl FnOnce(
            PluginId,
            Result<Option<Vec<CallHierarchyIncomingCall>>, RpcError>,
        ) + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let method = CallHierarchyIncomingCalls::METHOD;
        let params = CallHierarchyIncomingCallsParams {
            item,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
        };

        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn show_call_hierarchy(
        &self,
        path: &Path,
        position: Position,
        cb: impl FnOnce(PluginId, Result<Option<Vec<CallHierarchyItem>>, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = CallHierarchyPrepare::METHOD;
        let params = CallHierarchyPrepareParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params:     WorkDoneProgressParams::default(),
        };

        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn document_highlight(
        &self,
        path: &Path,
        position: Position,
        cb: impl FnOnce(PluginId, Result<Option<Vec<DocumentHighlight>>, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = DocumentHighlightRequest::METHOD;
        let params = DocumentHighlightParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params:     Default::default(),
            partial_result_params:         Default::default(),
        };

        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn get_references(
        &self,
        path: &Path,
        position: Position,
        cb: impl FnOnce(PluginId, Result<Vec<Location>, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = References::METHOD;
        let params = ReferenceParams {
            text_document_position:    TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params:     PartialResultParams::default(),
            context:                   ReferenceContext {
                include_declaration: false,
            },
        };

        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn get_lsp_folding_range(
        &self,
        path: &Path,
        cb: impl FnOnce(
            PluginId,
            std::result::Result<Option<Vec<FoldingRange>>, RpcError>,
        ) + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = FoldingRangeRequest::METHOD;
        let params = FoldingRangeParams {
            text_document:             TextDocumentIdentifier { uri },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params:     PartialResultParams::default(),
        };

        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn go_to_implementation(
        &self,
        path: &Path,
        position: Position,
        cb: impl FnOnce(PluginId, Result<Option<GotoImplementationResponse>, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = GotoImplementation::METHOD;
        let params = GotoTypeDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params:     WorkDoneProgressParams::default(),
            partial_result_params:         PartialResultParams::default(),
        };

        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn get_code_actions(
        &self,
        path: &Path,
        position: Position,
        diagnostics: Vec<Diagnostic>,
        cb: impl FnOnce(PluginId, Result<CodeActionResponse, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = CodeActionRequest::METHOD;
        let params = CodeActionParams {
            text_document:             TextDocumentIdentifier { uri },
            range:                     Range {
                start: position,
                end:   position,
            },
            context:                   CodeActionContext {
                diagnostics,
                only: None,
                trigger_kind: None,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params:     PartialResultParams::default(),
        };
        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn get_code_lens(
        &self,
        path: &Path,
        cb: impl FnOnce(PluginId, Result<Option<Vec<CodeLens>>, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = CodeLensRequest::METHOD;
        let params = CodeLensParams {
            text_document:             TextDocumentIdentifier { uri },
            work_done_progress_params: Default::default(),
            partial_result_params:     Default::default(),
        };

        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());

        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn get_code_lens_resolve(
        &self,
        path: &Path,
        code_lens: &CodeLens,
        cb: impl FnOnce(PluginId, Result<CodeLens, RpcError>) + Clone + Send + 'static,
        id: u64,
    ) {
        let method = CodeLensResolve::METHOD;
        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());

        self.send_request_to_all_plugins(
            method,
            code_lens,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn get_inlay_hints(
        &self,
        path: &Path,
        range: Range,
        cb: impl FnOnce(PluginId, Result<Vec<InlayHint>, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = InlayHintRequest::METHOD;
        let params = InlayHintParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: WorkDoneProgressParams::default(),
            range,
        };
        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn get_inline_completions(
        &self,
        path: &Path,
        position: Position,
        trigger_kind: InlineCompletionTriggerKind,
        cb: impl FnOnce(PluginId, Result<InlineCompletionResponse, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = InlineCompletionRequest::METHOD;
        let params = InlineCompletionParams {
            text_document_position:    TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            context:                   lsp_types::InlineCompletionContext {
                trigger_kind,
                selected_completion_info: None,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn get_document_symbols(
        &self,
        path: &Path,
        cb: impl FnOnce(PluginId, Result<DocumentSymbolResponse, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = DocumentSymbolRequest::METHOD;
        let params = DocumentSymbolParams {
            text_document:             TextDocumentIdentifier { uri },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params:     PartialResultParams::default(),
        };
        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn get_workspace_symbols(
        &self,
        query: String,
        cb: impl FnOnce(PluginId, Result<Vec<SymbolInformation>, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let method = WorkspaceSymbolRequest::METHOD;
        let params = WorkspaceSymbolParams {
            query,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        self.send_request_to_all_plugins(method, params, None, None, id, cb);
    }

    pub fn get_document_formatting(
        &self,
        path: &Path,
        cb: impl FnOnce(PluginId, Result<Vec<TextEdit>, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = Formatting::METHOD;
        let params = DocumentFormattingParams {
            text_document:             TextDocumentIdentifier { uri },
            options:                   FormattingOptions {
                tab_size: 4,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn prepare_rename(
        &self,
        path: &Path,
        position: Position,
        cb: impl FnOnce(PluginId, Result<PrepareRenameResponse, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = PrepareRenameRequest::METHOD;
        let params = TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position,
        };
        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn rename(
        &self,
        path: &Path,
        position: Position,
        new_name: String,
        cb: impl FnOnce(PluginId, Result<WorkspaceEdit, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = Rename::METHOD;
        let params = RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            new_name,
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn get_semantic_tokens(
        &self,
        path: &Path,
        cb: impl FnOnce(PluginId, Result<SemanticTokens, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = SemanticTokensFullRequest::METHOD;
        let params = SemanticTokensParams {
            text_document:             TextDocumentIdentifier { uri },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params:     PartialResultParams::default(),
        };
        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn get_semantic_tokens_delta(
        &self,
        path: &Path,
        previous_result_id: String,
        cb: impl FnOnce(PluginId, Result<SemanticTokensFullDeltaResult, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = SemanticTokensFullDeltaRequest::METHOD;
        let params = SemanticTokensDeltaParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            previous_result_id,
        };
        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn get_selection_range(
        &self,
        path: &Path,
        positions: Vec<Position>,
        cb: impl FnOnce(PluginId, Result<Vec<SelectionRange>, RpcError>)
        + Clone
        + Send
        + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = SelectionRangeRequest::METHOD;
        let params = SelectionRangeParams {
            text_document: TextDocumentIdentifier { uri },
            positions,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
        };
        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn hover(
        &self,
        path: &Path,
        position: Position,
        cb: impl FnOnce(PluginId, Result<Hover, RpcError>) + Clone + Send + 'static,
        id: u64,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = HoverRequest::METHOD;
        let params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params:     WorkDoneProgressParams::default(),
        };
        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());

        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            cb,
        );
    }

    pub fn completion(
        &self,
        request_id: usize,
        id: u64,
        path: &Path,
        input: String,
        position: Position,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = Completion::METHOD;
        let params = CompletionParams {
            text_document_position:    TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params:     PartialResultParams::default(),
            context:                   None,
        };

        let core_rpc = self.core_rpc.clone();
        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());

        self.send_request_to_all_plugins(
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            id,
            move |plugin_id, result| match result {
                Ok(value) => {
                    if let Ok(resp) =
                        serde_json::from_value::<CompletionResponse>(value)
                    {
                        core_rpc
                            .completion_response(request_id, input, resp, plugin_id);
                    }
                },
                Err(err) => {
                    log::error!("{:?}", err);
                },
            },
        );
    }

    pub fn completion_resolve(
        &self,
        plugin_id: PluginId,
        item: CompletionItem,
        cb: impl FnOnce(Result<CompletionItem, RpcError>) + Send + Clone + 'static,
        id: u64,
    ) {
        let method = ResolveCompletionItem::METHOD;
        self.send_request(
            Some(plugin_id),
            None,
            method,
            item,
            None,
            None,
            true,
            id,
            move |_, _, result| {
                let result = match result {
                    Ok(value) => {
                        if let Ok(item) =
                            serde_json::from_value::<CompletionItem>(value)
                        {
                            Ok(item)
                        } else {
                            Err(RpcError {
                                code:    0,
                                message: "completion item deserialize error"
                                    .to_string(),
                            })
                        }
                    },
                    Err(e) => Err(e),
                };
                cb(result)
            },
        );
    }

    pub fn signature_help(
        &self,
        request_id: usize,
        id: u64,
        path: &Path,
        position: Position,
    ) {
        let uri = Url::from_file_path(path).unwrap();
        let method = SignatureHelpRequest::METHOD;
        let params = SignatureHelpParams {
            // TODO: We could provide more information about the signature for the
            // LSP to work with
            context: None,
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        };

        let core_rpc = self.core_rpc.clone();
        let language_id =
            Some(language_id_from_path(path).unwrap_or("").to_string());
        self.send_request(
            None,
            None,
            method,
            params,
            language_id,
            Some(path.to_path_buf()),
            true,
            id,
            move |_, plugin_id, result| match result {
                Ok(value) => {
                    if let Ok(resp) = serde_json::from_value::<SignatureHelp>(value)
                    {
                        core_rpc
                            .signature_help_response(request_id, resp, plugin_id);
                    }
                },
                Err(err) => {
                    log::error!("{:?}", err);
                },
            },
        );
    }

    pub fn action_resolve(
        &self,
        item: CodeAction,
        plugin_id: PluginId,
        cb: impl FnOnce(Result<CodeAction, RpcError>) + Send + Clone + 'static,
        id: u64,
    ) {
        let method = CodeActionResolveRequest::METHOD;
        self.send_request(
            Some(plugin_id),
            None,
            method,
            item,
            None,
            None,
            true,
            id,
            move |_, _, result| {
                let result = match result {
                    Ok(value) => {
                        if let Ok(item) = serde_json::from_value::<CodeAction>(value)
                        {
                            Ok(item)
                        } else {
                            Err(RpcError {
                                code:    0,
                                message: "code_action item deserialize error"
                                    .to_string(),
                            })
                        }
                    },
                    Err(e) => Err(e),
                };
                cb(result)
            },
        );
    }

    pub fn did_open_document(
        &self,
        path: &Path,
        language_id: String,
        version: i32,
        text: String,
        id: u64,
    ) {
        match Url::from_file_path(path) {
            Ok(path) => {
                if let Err(err) =
                    self.plugin_tx.send(PluginCatalogRpc::DidOpenTextDocument {
                        document: TextDocumentItem::new(
                            path,
                            language_id,
                            version,
                            text,
                        ),
                        id,
                    })
                {
                    log::error!("{:?}", err);
                }
            },
            Err(_) => {
                log::error!("Failed to parse URL from file path: {path:?}");
            },
        }
    }

    pub fn unactivated_volts(
        &self,
        volts: Vec<VoltMetadata>,
        id: u64,
    ) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::UnactivatedVolts(
            volts, id,
        ))
    }

    pub fn plugin_server_loaded(
        &self,
        plugin: PluginServerRpcHandler,
    ) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::PluginServerLoaded(
            plugin,
        ))
    }

    pub fn update_plugin_configs(
        &self,
        configs: HashMap<String, HashMap<String, serde_json::Value>>,
    ) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::UpdatePluginConfigs(
            configs,
        ))
    }

    pub fn install_volt(&self, id: u64, volt: VoltInfo) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::InstallVolt(volt, id))
    }

    pub fn stop_volt(&self, id: u64, volt: VoltInfo) {
        let rpc = PluginCatalogRpc::RemoveVolt {
            volt,
            f: Box::new(|_, _id: PluginId, rs: Result<Value, RpcError>| {
                if let Err(e) = rs {
                    // maybe should send notification
                    error!("{:?}", e);
                }
            }),
            id,
        };
        if let Err(err) = self.plugin_tx.send(rpc) {
            log::error!("{:?}", err);
        }
    }

    pub fn remove_volt(&self, id: u64, volt: VoltMetadata) {
        let catalog_rpc = self.clone();
        let volt_clone = volt.clone();
        let rpc = PluginCatalogRpc::RemoveVolt {
            volt: volt.info(),
            f: Box::new(|_, _id: PluginId, rs: Result<Value, RpcError>| {
                if let Err(e) = rs {
                    // maybe should send notification
                    error!("{:?}", e);
                } else if let Err(e) = remove_volt(catalog_rpc, volt_clone) {
                    error!("{:?}", e);
                }
            }),
            id,
        };
        if let Err(err) = self.plugin_tx.send(rpc) {
            log::error!("{:?}", err);
        }
    }

    pub fn reload_volt(&self, id: u64, volt: VoltMetadata) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::ReloadVolt(volt, id))
    }

    pub fn enable_volt(&self, id: u64, volt: VoltInfo) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::EnableVolt(volt, id))
    }

    pub fn dap_disconnected(&self, dap_id: DapId) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::DapDisconnected(dap_id))
    }

    pub fn dap_loaded(&self, dap_rpc: DapRpcHandler) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::DapLoaded(dap_rpc))
    }

    pub fn dap_start(
        &self,
        config: RunDebugConfig,
        breakpoints: HashMap<PathBuf, Vec<SourceBreakpoint>>,
    ) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::DapStart {
            config,
            breakpoints,
        })
    }

    pub fn dap_process_id(
        &self,
        dap_id: DapId,
        process_id: Option<u32>,
        term_id: TermId,
    ) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::DapProcessId {
            dap_id,
            process_id,
            term_id,
        })
    }

    pub fn dap_continue(&self, dap_id: DapId, thread_id: ThreadId) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::DapContinue {
            dap_id,
            thread_id,
        })
    }

    pub fn dap_pause(&self, dap_id: DapId, thread_id: ThreadId) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::DapPause {
            dap_id,
            thread_id,
        })
    }

    pub fn dap_step_over(&self, dap_id: DapId, thread_id: ThreadId) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::DapStepOver {
            dap_id,
            thread_id,
        })
    }

    pub fn dap_step_into(&self, dap_id: DapId, thread_id: ThreadId) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::DapStepInto {
            dap_id,
            thread_id,
        })
    }

    pub fn dap_step_out(&self, dap_id: DapId, thread_id: ThreadId) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::DapStepOut {
            dap_id,
            thread_id,
        })
    }

    pub fn dap_stop(&self, dap_id: DapId) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::DapStop { dap_id })
    }

    pub fn dap_disconnect(&self, dap_id: DapId) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::DapDisconnect {
            dap_id,
        })
    }

    pub fn dap_restart(
        &self,
        config: RunDebugConfig,
        breakpoints: HashMap<PathBuf, Vec<SourceBreakpoint>>,
    ) -> Result<()> {
        self.catalog_notification(PluginCatalogNotification::DapRestart {
            config,
            breakpoints,
        })
    }

    pub fn dap_set_breakpoints(
        &self,
        dap_id: DapId,
        path: PathBuf,
        breakpoints: Vec<SourceBreakpoint>,
    ) -> Result<()> {
        log::info!(
            "dap_set_breakpoints dap_id={dap_id:?} path={path:?} \
             breakpoints={breakpoints:?}"
        );
        self.catalog_notification(PluginCatalogNotification::DapSetBreakpoints {
            dap_id,
            path,
            breakpoints,
        })
    }

    pub fn dap_variable(
        &self,
        dap_id: DapId,
        reference: usize,
        f: impl FnOnce(Id, Result<Vec<dap_types::Variable>, RpcError>) + Send + 'static,
    ) {
        if let Err(err) = self.plugin_tx.send(PluginCatalogRpc::DapVariable {
            dap_id,
            reference,
            f: Box::new(f),
        }) {
            log::error!("{:?}", err);
        }
    }

    pub fn dap_get_scopes(
        &self,
        dap_id: DapId,
        frame_id: usize,
        f: impl FnOnce(
            Id,
            Result<Vec<(dap_types::Scope, Vec<dap_types::Variable>)>, RpcError>,
        ) + Send
        + 'static,
    ) {
        if let Err(err) = self.plugin_tx.send(PluginCatalogRpc::DapGetScopes {
            dap_id,
            frame_id,
            f: Box::new(f),
        }) {
            log::error!("{:?}", err);
        }
    }

    pub fn register_debugger_type(
        &self,
        debugger_type: String,
        program: String,
        args: Option<Vec<String>>,
    ) {
        if let Err(err) = self.catalog_notification(
            PluginCatalogNotification::RegisterDebuggerType {
                debugger_type,
                program,
                args,
            },
        ) {
            log::error!("{:?}", err);
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "method", content = "params")]
pub enum PluginNotification {
    StartLspServer {
        exec_path:   String,
        language_id: String,
        options:     Option<Value>,
        system_lsp:  Option<bool>,
    },
    DownloadFile {
        url:  String,
        path: PathBuf,
    },
    LockFile {
        path: PathBuf,
    },
    MakeFileExecutable {
        path: PathBuf,
    },
}

/// todo remove
pub fn volt_icon(volt: &VoltMetadata) -> Option<Vec<u8>> {
    let dir = volt.dir.as_ref()?;
    let icon = dir.join(volt.icon.as_ref()?);
    std::fs::read(icon).ok()
}

pub async fn async_volt_icon(volt: &VoltMetadata) -> Option<Vec<u8>> {
    let dir = volt.dir.as_ref()?;
    let icon = dir.join(volt.icon.as_ref()?);
    tokio::fs::read(icon).await.ok()
}

pub async fn download_volt(
    volt: &VoltInfo,
    plugins_directory: &Path,
) -> Result<VoltMetadata> {
    let url = format!(
        "https://plugins.lapce.dev/api/v1/plugins/{}/{}/{}/download",
        volt.author, volt.name, volt.version
    );

    let resp = crate::async_get_url(url, None).await?;
    if !resp.status().is_success() {
        return Err(anyhow!("can't download plugin"));
    }

    // this is the s3 url
    let url = resp.text().await?;

    let resp = crate::async_get_url(url, None).await?;
    if !resp.status().is_success() {
        return Err(anyhow!("can't download plugin"));
    }

    let is_zstd = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        == Some("application/zstd");

    let id = volt.id();

    let plugin_dir = plugins_directory.join(id.to_string());
    if plugin_dir.exists() {
        tokio::fs::remove_dir_all(&plugin_dir).await?;
    }
    tokio::fs::create_dir_all(&plugin_dir).await?;

    let bytes = resp.bytes().await?;
    if is_zstd {
        let tar = zstd::Decoder::new(bytes.as_ref()).unwrap();
        let mut archive = Archive::new(tar);
        archive.unpack(&plugin_dir)?;
    } else {
        let tar = GzDecoder::new(bytes.as_ref());
        let mut archive = Archive::new(tar);
        archive.unpack(&plugin_dir)?;
    }

    let meta = load_volt(&plugin_dir).await?;
    Ok(meta)
}

pub async fn install_volt(
    catalog_rpc: PluginCatalogRpcHandler,
    workspace: Option<PathBuf>,
    configurations: Option<HashMap<String, serde_json::Value>>,
    volt: VoltInfo,
    id: u64,
    plugins_directory: PathBuf,
) -> Result<()> {
    let download_volt_result = download_volt(&volt, &plugins_directory).await;
    if download_volt_result.is_err() {
        catalog_rpc
            .core_rpc
            .volt_installing(volt, "Could not download Plugin".to_string());
    }
    let meta = download_volt_result?;
    let local_catalog_rpc = catalog_rpc.clone();
    let local_meta = meta.clone();

    if let Err(err) =
        start_volt(workspace, configurations, local_catalog_rpc, local_meta, id)
    {
        log::error!("{:?}", err);
    }
    let icon = volt_icon(&meta);
    catalog_rpc.core_rpc.volt_installed(meta, icon);
    Ok(())
}

pub fn remove_volt(
    catalog_rpc: PluginCatalogRpcHandler,
    volt: VoltMetadata,
) -> Result<()> {
    std::thread::spawn(move || -> Result<()> {
        let path = volt.dir.as_ref().ok_or_else(|| {
            catalog_rpc
                .core_rpc
                .volt_removing(volt.clone(), "Plugin Directory not set".to_string());
            anyhow::anyhow!("don't have dir")
        })?;
        let mut rs = Ok(());
        // Try to remove dir
        // This is due to some operating systems not releasing immediately, such as
        // Windows.
        for _ in 0..2 {
            rs = std::fs::remove_dir_all(path);
            if rs.is_err() {
                std::thread::sleep(Duration::from_millis(500));
            } else {
                break;
            }
        }
        if let Err(e) = rs {
            error!("remove_dir_all {:?}", e);
            eprintln!("Could not delete plugin folder: {e}");
            catalog_rpc.core_rpc.volt_removing(
                volt.clone(),
                "Could not remove Plugin Directory".to_string(),
            );
        } else {
            catalog_rpc.core_rpc.volt_removed(volt.info(), false);
        }
        Ok(())
    });
    Ok(())
}

fn client_capabilities() -> ClientCapabilities {
    // https://github.com/rust-lang/rust-analyzer/blob/master/docs/dev/lsp-extensions.md#server-status
    let mut experimental = Map::new();
    experimental.insert("serverStatusNotification".into(), true.into());
    let command_vec = ["rust-analyzer.runSingle", "rust-analyzer.debugSingle"]
        .map(Value::from)
        .to_vec();

    let mut commands = Map::new();
    experimental.insert("serverStatusNotification".into(), true.into());
    commands.insert("commands".into(), command_vec.into());
    experimental.insert("commands".into(), commands.into());
    ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            synchronization: Some(TextDocumentSyncClientCapabilities {
                did_save: Some(true),
                dynamic_registration: Some(true),
                ..Default::default()
            }),
            document_highlight: Some(DocumentHighlightClientCapabilities {
                ..Default::default()
            }),
            completion: Some(CompletionClientCapabilities {
                completion_item: Some(CompletionItemCapability {
                    snippet_support: Some(true),
                    resolve_support: Some(CompletionItemCapabilityResolveSupport {
                        properties: vec!["additionalTextEdits".to_string()],
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            signature_help: Some(SignatureHelpClientCapabilities {
                signature_information: Some(SignatureInformationSettings {
                    documentation_format:     Some(vec![
                        MarkupKind::Markdown,
                        MarkupKind::PlainText,
                    ]),
                    parameter_information:    Some(ParameterInformationSettings {
                        label_offset_support: Some(true),
                    }),
                    active_parameter_support: Some(true),
                }),
                ..Default::default()
            }),
            hover: Some(HoverClientCapabilities {
                content_format: Some(vec![
                    MarkupKind::Markdown,
                    MarkupKind::PlainText,
                ]),
                ..Default::default()
            }),
            inlay_hint: Some(InlayHintClientCapabilities {
                ..Default::default()
            }),
            code_action: Some(CodeActionClientCapabilities {
                data_support: Some(true),
                resolve_support: Some(CodeActionCapabilityResolveSupport {
                    properties: vec!["edit".to_string()],
                }),
                code_action_literal_support: Some(CodeActionLiteralSupport {
                    code_action_kind: CodeActionKindLiteralSupport {
                        value_set: vec![
                            CodeActionKind::EMPTY.as_str().to_string(),
                            CodeActionKind::QUICKFIX.as_str().to_string(),
                            CodeActionKind::REFACTOR.as_str().to_string(),
                            CodeActionKind::REFACTOR_EXTRACT.as_str().to_string(),
                            CodeActionKind::REFACTOR_INLINE.as_str().to_string(),
                            CodeActionKind::REFACTOR_REWRITE.as_str().to_string(),
                            CodeActionKind::SOURCE.as_str().to_string(),
                            CodeActionKind::SOURCE_ORGANIZE_IMPORTS
                                .as_str()
                                .to_string(),
                            "quickassist".to_string(),
                            "source.fixAll".to_string(),
                        ],
                    },
                }),
                ..Default::default()
            }),
            semantic_tokens: Some(SemanticTokensClientCapabilities {
                requests: SemanticTokensClientCapabilitiesRequests {
                    range: None,
                    full:  Some(SemanticTokensFullOptions::Delta {
                        delta: Some(true),
                    }),
                },
                ..Default::default()
            }),
            type_definition: Some(GotoCapability {
                // Note: This is explicitly specified rather than left to the Default
                // because of a bug in lsp-types https://github.com/gluon-lang/lsp-types/pull/244
                link_support: Some(false),
                ..Default::default()
            }),
            definition: Some(GotoCapability {
                ..Default::default()
            }),
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                ..Default::default()
            }),
            inline_completion: Some(InlineCompletionClientCapabilities {
                ..Default::default()
            }),
            call_hierarchy: Some(CallHierarchyClientCapabilities {
                dynamic_registration: Some(true),
            }),
            document_symbol: Some(DocumentSymbolClientCapabilities {
                hierarchical_document_symbol_support: Some(true),
                ..Default::default()
            }),
            folding_range: Some(FoldingRangeClientCapabilities {
                dynamic_registration: Some(false),
                range_limit:          None,
                line_folding_only:    Some(false),
                folding_range_kind:   None,
                folding_range:        None,
            }),
            ..Default::default()
        }),
        window: Some(WindowClientCapabilities {
            work_done_progress: Some(true),
            show_message: Some(ShowMessageRequestClientCapabilities {
                message_action_item: Some(MessageActionItemCapabilities {
                    additional_properties_support: Some(true),
                }),
            }),
            ..Default::default()
        }),
        workspace: Some(WorkspaceClientCapabilities {
            symbol: Some(WorkspaceSymbolClientCapabilities {
                ..Default::default()
            }),
            configuration: Some(false),
            workspace_folders: Some(true),
            ..Default::default()
        }),
        experimental: Some(experimental.into()),
        ..Default::default()
    }
}
