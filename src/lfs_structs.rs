//! Data structures for the Git LFS HTTP API.
//!
//! These types encode/decode the JSON payloads exchanged with an LFS server: batch
//! requests, transfer adapter selection, signed action URLs (download/upload/verify),
//! file locks, and chunked transfer metadata.
//!
//! All structs match the wire format defined by the LFS spec
//! (<https://github.com/git-lfs/git-lfs/blob/main/docs/api>) and rely on `serde` rename
//! attributes to bridge `snake_case` Rust identifiers with the API's `lowercase`
//! conventions. None of these types perform I/O; they are pure data carriers used by
//! [`crate::internal::protocol::lfs_client`] and [`crate::command::lfs`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Negotiated transfer adapter for a batch request.
///
/// The LFS server advertises which adapters it supports; clients echo back the one
/// they want to use. `BASIC` is the only adapter every server must implement.
#[derive(Serialize, Deserialize, Debug, Default)]
pub enum TransferMode {
    /// Single-shot download/upload through the URL returned in `actions`.
    #[default]
    #[serde(rename = "basic")]
    BASIC,
    /// Object split into discrete pieces, each with its own URL — typically used for
    /// objects larger than the configured chunk threshold.
    #[serde(rename = "multipart")]
    MULTIPART,
    /// Streaming uploads via TUS-like resumable PATCH semantics. Reserved by the spec
    /// but not yet implemented in Libra.
    STREAMING,
}

/// Direction of an LFS batch request.
#[derive(Serialize, Deserialize, PartialEq, Eq, Hash, Debug, Clone)]
pub enum Operation {
    /// Server-to-client: fetch object content.
    #[serde(rename = "download")]
    Download,
    /// Client-to-server: push object content.
    #[serde(rename = "upload")]
    Upload,
}

/// Download operations MUST specify a download action, or an object error if the object cannot be downloaded for some reason.
/// Upload operations can specify an upload and a verify action.
/// The upload action describes how to upload the object. If the object has a verify action, the LFS client will hit this URL after a successful upload. Servers can use this for extra verification, if needed.
/// If a client requests to upload an object that the server already has, the server should omit the actions property completely. The client will then assume the server already has it.
#[derive(Serialize, Deserialize, PartialEq, Eq, Hash, Debug)]
pub enum Action {
    #[serde(rename = "download")]
    Download,
    #[serde(rename = "upload")]
    Upload,
    #[serde(rename = "verify")]
    Verify,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct RequestObject {
    pub oid: String,
    pub size: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub authorization: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Lock {
    pub id: String,
    pub path: String,
    pub locked_at: String,
    pub owner: Option<User>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct User {
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BatchRequest {
    // Should be download or upload.
    pub operation: Operation,
    // An optional Array of String identifiers for transfer adapters that the client has configured.
    // If omitted, the basic transfer adapter MUST be assumed by the server.
    pub transfers: Vec<String>,
    pub objects: Vec<RequestObject>,
    pub hash_algo: String,
}

#[derive(Serialize, Deserialize)]
pub struct BatchResponse {
    pub transfer: TransferMode,
    pub objects: Vec<ResponseObject>,
    pub hash_algo: String,
}

#[derive(Serialize, Deserialize)]
pub struct FetchchunkResponse {
    pub oid: String,
    pub size: i64,
    pub chunks: Vec<ChunkDownloadObject>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Link {
    pub href: String,
    #[serde(default)] // Optional field
    pub header: HashMap<String, String>,
    pub expires_at: String,
}

impl Link {
    /// Build a [`Link`] for an LFS action URL with sensible defaults.
    ///
    /// Functional scope:
    /// - Sets the `Accept: application/vnd.git-lfs` header so downstream HTTP clients
    ///   negotiate the LFS media type without having to remember it.
    /// - Stamps `expires_at` 24 hours into the future (RFC 3339), the default LFS
    ///   action lifetime expected by Git LFS clients.
    ///
    /// Boundary conditions:
    /// - `href` is stored verbatim; callers are responsible for URL encoding.
    /// - The 24-hour expiry is not configurable here; servers that wish to issue
    ///   shorter-lived URLs should construct the struct manually.
    pub fn new(href: &str) -> Self {
        let mut header = HashMap::new();
        header.insert("Accept".to_string(), "application/vnd.git-lfs".to_owned());

        Link {
            href: href.to_string(),
            header,
            expires_at: {
                use chrono::{DateTime, Duration, Utc};
                // INVARIANT: 86_400 is a small constant well inside chrono's
                // representable range for Duration seconds; `try_seconds`
                // only returns None for values that overflow i64 nanoseconds
                // when multiplied by 1_000_000_000.
                let expire_time: DateTime<Utc> = Utc::now()
                    + Duration::try_seconds(86400)
                        .expect("24h in seconds is representable as chrono::Duration");
                expire_time.to_rfc3339()
            },
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct ObjectError {
    pub code: i64,
    pub message: String,
}

#[derive(Serialize, Deserialize)]
pub struct ResponseObject {
    pub oid: String,
    pub size: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authenticated: Option<bool>,
    // Object containing the next actions for this object. Applicable actions depend on which operation is specified in the request.
    // How these properties are interpreted depends on which transfer adapter the client will be using.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<HashMap<Action, Link>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ObjectError>,
}

pub struct ResCondition {
    pub file_exist: bool,
    pub operation: Operation,
    pub use_tus: bool,
}

impl ResponseObject {
    /// Build a [`ResponseObject`] for the four `(file_exist, operation)` combinations
    /// defined by the LFS batch API.
    ///
    /// Functional scope, by `res_condition`:
    /// - `(file_exist=true, Upload)`: omit `actions` entirely so the client knows the
    ///   server already has the object and skips the upload — required by spec.
    /// - `(file_exist=true, Download)`: emit a single `Download` action pointing at
    ///   `download_url`.
    /// - `(file_exist=false, Upload)`: emit a single `Upload` action pointing at
    ///   `upload_url`. (TUS verification is wired up but currently disabled — see the
    ///   commented-out block in source.)
    /// - `(file_exist=false, Download)`: cannot serve the object; populate `error`
    ///   with HTTP-style code 404.
    ///
    /// Boundary conditions:
    /// - `meta.oid` and `meta.size` are echoed back verbatim so the LFS client can
    ///   correlate the response with its own request even when reordering occurs.
    /// - `authenticated` is always `Some(true)` because Libra only returns response
    ///   objects after the surrounding handler has authenticated the caller.
    pub fn new(
        meta: &MetaObject,
        res_condition: ResCondition,
        download_url: &str,
        upload_url: &str,
    ) -> ResponseObject {
        let mut res = ResponseObject {
            oid: meta.oid.to_owned(),
            size: meta.size,
            authenticated: Some(true),
            actions: None,
            error: None,
        };

        let mut actions = HashMap::new();

        match res_condition {
            ResCondition {
                file_exist: true,
                operation: Operation::Upload,
                ..
            } => {
                //If a client requests to upload an object that the server already has, the server should omit the actions property completely.
                // The client will then assume the server already has it.
                tracing::debug!("File existing, leave actions empty")
            }
            ResCondition {
                file_exist: true,
                operation: Operation::Download,
                ..
            } => {
                actions.insert(Action::Download, Link::new(download_url));
                res.actions = Some(actions);
            }
            ResCondition {
                file_exist: false,
                operation: Operation::Upload,
                ..
            } => {
                actions.insert(Action::Upload, Link::new(upload_url));
                // if use_tus {
                //     actions.insert(
                //         Action::Verify,
                //         Link::new(&req_object.verify_link(hostname.to_string())),
                //     );
                // }
                res.actions = Some(actions);
            }
            ResCondition {
                file_exist: false,
                operation: Operation::Download,
                ..
            } => {
                let err = ObjectError {
                    code: 404,
                    message: "Not found".to_owned(),
                };
                res.error = Some(err)
            }
        }
        res
    }

    /// Construct a failure-only response for `object` carrying `err`.
    ///
    /// Used when the server cannot even compute a [`MetaObject`] (e.g. the OID is
    /// malformed or storage is unreachable), so the normal [`ResponseObject::new`]
    /// path cannot run.
    pub fn failed_with_err(object: &RequestObject, err: ObjectError) -> ResponseObject {
        ResponseObject {
            oid: object.oid.to_owned(),
            size: object.size,
            authenticated: None,
            actions: None,
            error: Some(err),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct ChunkDownloadObject {
    pub sub_oid: String,
    pub offset: i64,
    pub size: i64,
    pub link: Link,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Ref {
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct LockRequest {
    pub path: String,
    #[serde(rename(serialize = "ref", deserialize = "ref"))]
    pub refs: Ref,
}

#[derive(Serialize, Deserialize)]
pub struct LockResponse {
    pub lock: Lock,
    pub message: String,
}

#[derive(Serialize, Deserialize, Default)]
pub struct UnlockRequest {
    pub force: Option<bool>,
    #[serde(rename(serialize = "ref", deserialize = "ref"))]
    pub refs: Ref,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct UnlockResponse {
    pub lock: Lock,
    pub message: String,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct LockList {
    pub locks: Vec<Lock>,
    pub next_cursor: String,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct VerifiableLockRequest {
    #[serde(rename(serialize = "ref", deserialize = "ref"))]
    pub refs: Ref,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct VerifiableLockList {
    pub ours: Vec<Lock>,
    pub theirs: Vec<Lock>,
    pub next_cursor: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct LockListQuery {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub cursor: String,
    #[serde(default)]
    pub limit: String,
    #[serde(default)]
    pub refspec: String,
}

// Define MetaObject as it's used in ResponseObject::new
#[derive(Debug, Clone)]
pub struct MetaObject {
    pub oid: String,
    pub size: i64,
    pub exist: bool,
    pub splited: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── TransferMode serde ───────────────────────────────────────────

    #[test]
    fn transfer_mode_default_is_basic() {
        let mode = TransferMode::default();
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, "\"basic\"");
    }

    #[test]
    fn transfer_mode_roundtrip_basic() {
        let json = "\"basic\"";
        let mode: TransferMode = serde_json::from_str(json).unwrap();
        assert!(matches!(mode, TransferMode::BASIC));
    }

    #[test]
    fn transfer_mode_roundtrip_multipart() {
        let json = "\"multipart\"";
        let mode: TransferMode = serde_json::from_str(json).unwrap();
        assert!(matches!(mode, TransferMode::MULTIPART));
    }

    // ── Operation serde ──────────────────────────────────────────────

    #[test]
    fn operation_roundtrip() {
        let dl = serde_json::to_string(&Operation::Download).unwrap();
        assert_eq!(dl, "\"download\"");
        let ul = serde_json::to_string(&Operation::Upload).unwrap();
        assert_eq!(ul, "\"upload\"");

        let parsed: Operation = serde_json::from_str("\"download\"").unwrap();
        assert_eq!(parsed, Operation::Download);
    }

    // ── Action serde ─────────────────────────────────────────────────

    #[test]
    fn action_serde_all_variants() {
        for (variant, expected) in [
            (Action::Download, "\"download\""),
            (Action::Upload, "\"upload\""),
            (Action::Verify, "\"verify\""),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected);
        }
    }

    // ── RequestObject defaults ───────────────────────────────────────

    #[test]
    fn request_object_default_has_empty_optional_fields() {
        let obj = RequestObject::default();
        assert!(obj.oid.is_empty());
        assert_eq!(obj.size, 0);
        assert!(obj.user.is_empty());
        assert!(obj.password.is_empty());
        assert!(obj.repo.is_empty());
        assert!(obj.authorization.is_empty());
    }

    #[test]
    fn request_object_skips_empty_fields_in_json() {
        let obj = RequestObject {
            oid: "abc123".to_string(),
            size: 42,
            ..Default::default()
        };
        let json = serde_json::to_string(&obj).unwrap();
        assert!(json.contains("\"oid\":\"abc123\""));
        assert!(json.contains("\"size\":42"));
        // Empty fields should be skipped by skip_serializing_if
        assert!(!json.contains("\"user\""));
        assert!(!json.contains("\"password\""));
    }

    // ── Link::new ────────────────────────────────────────────────────

    #[test]
    fn link_new_sets_accept_header() {
        let link = Link::new("https://example.com/lfs/obj");
        assert_eq!(link.href, "https://example.com/lfs/obj");
        assert_eq!(
            link.header.get("Accept").map(String::as_str),
            Some("application/vnd.git-lfs"),
        );
    }

    #[test]
    fn link_new_sets_rfc3339_expiry() {
        let link = Link::new("https://example.com");
        // The expires_at should be a valid RFC 3339 timestamp
        assert!(
            chrono::DateTime::parse_from_rfc3339(&link.expires_at).is_ok(),
            "expires_at should be valid RFC 3339: {}",
            link.expires_at,
        );
    }

    // ── ResponseObject::new — all four (file_exist, operation) combos ──

    fn sample_meta(exist: bool) -> MetaObject {
        MetaObject {
            oid: "deadbeef".to_string(),
            size: 1024,
            exist,
            splited: false,
        }
    }

    #[test]
    fn response_object_existing_upload_omits_actions() {
        let meta = sample_meta(true);
        let res = ResponseObject::new(
            &meta,
            ResCondition {
                file_exist: true,
                operation: Operation::Upload,
                use_tus: false,
            },
            "",
            "",
        );
        assert_eq!(res.oid, "deadbeef");
        assert_eq!(res.size, 1024);
        assert!(res.actions.is_none());
        assert!(res.error.is_none());
    }

    #[test]
    fn response_object_existing_download_has_download_action() {
        let meta = sample_meta(true);
        let res = ResponseObject::new(
            &meta,
            ResCondition {
                file_exist: true,
                operation: Operation::Download,
                use_tus: false,
            },
            "https://dl.example.com/obj",
            "",
        );
        let actions = res.actions.as_ref().expect("download action required");
        assert!(actions.contains_key(&Action::Download));
        assert_eq!(
            actions[&Action::Download].href,
            "https://dl.example.com/obj"
        );
    }

    #[test]
    fn response_object_missing_upload_has_upload_action() {
        let meta = sample_meta(false);
        let res = ResponseObject::new(
            &meta,
            ResCondition {
                file_exist: false,
                operation: Operation::Upload,
                use_tus: false,
            },
            "",
            "https://ul.example.com/obj",
        );
        let actions = res.actions.as_ref().expect("upload action required");
        assert!(actions.contains_key(&Action::Upload));
        assert_eq!(actions[&Action::Upload].href, "https://ul.example.com/obj");
    }

    #[test]
    fn response_object_missing_download_has_404_error() {
        let meta = sample_meta(false);
        let res = ResponseObject::new(
            &meta,
            ResCondition {
                file_exist: false,
                operation: Operation::Download,
                use_tus: false,
            },
            "",
            "",
        );
        assert!(res.actions.is_none());
        let err = res
            .error
            .as_ref()
            .expect("error expected for missing download");
        assert_eq!(err.code, 404);
    }

    // ── ResponseObject::failed_with_err ──────────────────────────────

    #[test]
    fn failed_with_err_carries_error_and_echoes_oid() {
        let req = RequestObject {
            oid: "badf00d".to_string(),
            size: 999,
            ..Default::default()
        };
        let err = ObjectError {
            code: 500,
            message: "storage unreachable".to_string(),
        };
        let res = ResponseObject::failed_with_err(&req, err);
        assert_eq!(res.oid, "badf00d");
        assert_eq!(res.size, 999);
        assert!(res.authenticated.is_none());
        assert!(res.actions.is_none());
        let e = res.error.expect("error must be set");
        assert_eq!(e.code, 500);
        assert_eq!(e.message, "storage unreachable");
    }

    // ── BatchRequest serde ───────────────────────────────────────────

    #[test]
    fn batch_request_roundtrip() {
        let req = BatchRequest {
            operation: Operation::Download,
            transfers: vec!["basic".to_string()],
            objects: vec![RequestObject {
                oid: "aabbcc".to_string(),
                size: 100,
                ..Default::default()
            }],
            hash_algo: "sha256".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: BatchRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.operation, Operation::Download);
        assert_eq!(parsed.objects.len(), 1);
        assert_eq!(parsed.objects[0].oid, "aabbcc");
        assert_eq!(parsed.hash_algo, "sha256");
    }

    // ── Lock / LockRequest serde ─────────────────────────────────────

    #[test]
    fn lock_request_uses_ref_rename() {
        let req = LockRequest {
            path: "path/to/file".to_string(),
            refs: Ref {
                name: "refs/heads/main".to_string(),
            },
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            json.contains("\"ref\""),
            "refs field should serialize as 'ref'"
        );
        assert!(!json.contains("\"refs\""));
    }

    #[test]
    fn lock_list_default_is_empty() {
        let ll = LockList::default();
        assert!(ll.locks.is_empty());
        assert!(ll.next_cursor.is_empty());
    }

    // ── LockListQuery defaults ───────────────────────────────────────

    #[test]
    fn lock_list_query_deserializes_with_defaults() {
        let json = "{}";
        let q: LockListQuery = serde_json::from_str(json).unwrap();
        assert!(q.path.is_empty());
        assert!(q.id.is_empty());
        assert!(q.cursor.is_empty());
        assert!(q.limit.is_empty());
        assert!(q.refspec.is_empty());
    }
}
