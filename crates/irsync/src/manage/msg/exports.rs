//! Edits of the exports in the config message for `export add` and
//! `export remove`. Pure on a message; loading and saving belong to the
//! caller.

use crate::config::exports::RESERVED;
use crate::manage::exports::unique_auto_name;
use crate::manage::ManageError;
use iroh::EndpointId;
use irsync_proto::config as pb;
use std::path::Path;

/// Add export `name` with `path` and the flags that are true, last.
pub fn add_export(
    msg: &mut pb::Config,
    name: &str,
    path: &Path,
    read_only: bool,
    warm_index: bool,
) -> Result<(), ManageError> {
    if RESERVED.contains(&name) {
        return Err(ManageError::Reserved(name.to_string()));
    }
    if msg.exports.iter().any(|e| e.name == name) {
        return Err(ManageError::Exists(name.to_string()));
    }
    msg.exports.push(pb::Export {
        name: name.to_string(),
        path: path.to_string_lossy().into_owned(),
        read_only: read_only.then_some(true),
        warm_index: warm_index.then_some(true),
        ..Default::default()
    });
    Ok(())
}

/// Add an automatic export of `path`, read-write for `peer`; the name it
/// got (`-2`, `-3`, ... when the name is taken).
pub fn add_auto_export(
    msg: &mut pb::Config,
    path: &Path,
    peer: EndpointId,
) -> Result<String, ManageError> {
    let name = unique_auto_name(path, |n| msg.exports.iter().any(|e| e.name == n));
    msg.exports.push(pb::Export {
        name: name.clone(),
        path: path.to_string_lossy().into_owned(),
        auto: Some(true),
        rw: vec![pb::Entry {
            value: peer.to_string(),
            comment: None,
        }],
        rw_set: true,
        ..Default::default()
    });
    Ok(name)
}

/// Remove export `name` together with its grants.
pub fn remove_export(msg: &mut pb::Config, name: &str) -> Result<(), ManageError> {
    let i = msg
        .exports
        .iter()
        .position(|e| e.name == name)
        .ok_or_else(|| ManageError::NotFound(name.to_string()))?;
    msg.exports.remove(i);
    Ok(())
}
