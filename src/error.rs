//! Stable, versioned errors crossing the Remote wire/UI boundary.
//!
//! Producers should construct an [`ErrorEnvelope`] with one of the registered
//! codes below.  Human-readable text belongs in the local UI catalogue and is
//! never used for control flow.

use crate::message_proto::{ErrorCategory, ErrorCode, ErrorEnvelope};

pub const ERROR_SCHEMA_VERSION: u32 = 1;
pub const MAX_ERROR_DETAILS_BYTES: usize = 1024;
pub const MAX_CORRELATION_ID_BYTES: usize = 128;

pub fn envelope(
    category: ErrorCategory,
    code: ErrorCode,
    retryable: bool,
    display_key: &'static str,
) -> ErrorEnvelope {
    ErrorEnvelope {
        schema_version: ERROR_SCHEMA_VERSION,
        category: category.into(),
        code: code.into(),
        retryable,
        display_key: display_key.to_owned(),
        ..Default::default()
    }
}

pub fn with_details(mut error: ErrorEnvelope, details: impl AsRef<str>) -> ErrorEnvelope {
    error.details = bounded(details.as_ref(), MAX_ERROR_DETAILS_BYTES);
    error
}

pub fn with_correlation_id(
    mut error: ErrorEnvelope,
    correlation_id: impl AsRef<str>,
) -> ErrorEnvelope {
    error.correlation_id = bounded(correlation_id.as_ref(), MAX_CORRELATION_ID_BYTES);
    error
}

pub fn bounded(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

pub fn is_valid(error: &ErrorEnvelope) -> bool {
    error.schema_version == ERROR_SCHEMA_VERSION
        && error.category != ErrorCategory::ERROR_CATEGORY_UNSPECIFIED.into()
        && error.code != ErrorCode::ERROR_CODE_UNSPECIFIED.into()
        && error.display_key.len() <= 128
        && error.details.len() <= MAX_ERROR_DETAILS_BYTES
        && error.correlation_id.len() <= MAX_CORRELATION_ID_BYTES
        && !error.display_key.chars().any(char::is_control)
        && !error.details.chars().any(char::is_control)
        && !error.correlation_id.chars().any(char::is_control)
}

/// Convert a stable error code to its contract defaults. Producers may add
/// bounded details/correlation after calling this helper.
pub fn for_code(code: ErrorCode) -> ErrorEnvelope {
    use ErrorCategory::*;
    let (category, retryable, display_key) = match code {
        ErrorCode::PASSWORD_REQUIRED => (AUTHENTICATION, true, "password_required"),
        ErrorCode::PASSWORD_INVALID => (AUTHENTICATION, true, "password_invalid"),
        ErrorCode::TWO_FACTOR_REQUIRED => (AUTHENTICATION, true, "two_factor_required"),
        ErrorCode::TWO_FACTOR_INVALID => (AUTHENTICATION, true, "two_factor_invalid"),
        ErrorCode::NO_PASSWORD_ACCESS => (AUTHORIZATION, true, "no_password_access"),
        ErrorCode::WAYLAND_UNSUPPORTED => (PLATFORM, true, "wayland_unsupported"),
        ErrorCode::DISPLAY_SERVER_UNSUPPORTED => (PLATFORM, true, "display_server_unsupported"),
        ErrorCode::CONNECTION_OFFLINE => (NETWORK, false, "connection_offline"),
        ErrorCode::CONNECTION_TIMEOUT => (NETWORK, true, "connection_timeout"),
        ErrorCode::CONNECTION_RESET => (NETWORK, true, "connection_reset"),
        ErrorCode::CONNECTION_CLOSED => (NETWORK, false, "connection_closed"),
        ErrorCode::PERMISSION_DENIED => (AUTHORIZATION, false, "permission_denied"),
        ErrorCode::CONFLICT => (STATE, false, "conflict"),
        ErrorCode::CANCELLED => (STATE, false, "cancelled"),
        ErrorCode::NOT_FOUND => (STATE, false, "not_found"),
        ErrorCode::FILE_OPERATION_FAILED => (FILE_TRANSFER, false, "file_operation_failed"),
        ErrorCode::HTTP_PROXY_FAILED => (NETWORK, true, "http_proxy_failed"),
        ErrorCode::PROTOCOL_MISMATCH => (PROTOCOL, false, "protocol_mismatch"),
        ErrorCode::INTERNAL_FAILURE | ErrorCode::ERROR_CODE_UNSPECIFIED => {
            (INTERNAL, false, "internal_failure")
        }
    };
    envelope(category, code, retryable, display_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_is_versioned_and_bounded() {
        let error = with_correlation_id(
            with_details(
                envelope(
                    ErrorCategory::NETWORK,
                    ErrorCode::CONNECTION_TIMEOUT,
                    true,
                    "connection_timeout",
                ),
                "x".repeat(MAX_ERROR_DETAILS_BYTES + 10),
            ),
            "trace-1",
        );
        assert!(is_valid(&error));
        assert_eq!(error.schema_version, ERROR_SCHEMA_VERSION);
        assert_eq!(error.details.len(), MAX_ERROR_DETAILS_BYTES);
    }

    #[test]
    fn rejects_unknown_version_and_control_data() {
        let mut error = envelope(
            ErrorCategory::AUTHENTICATION,
            ErrorCode::PASSWORD_INVALID,
            false,
            "password_invalid",
        );
        assert!(is_valid(&error));
        error.schema_version += 1;
        assert!(!is_valid(&error));
        error.schema_version = ERROR_SCHEMA_VERSION;
        error.display_key.push('\n');
        assert!(!is_valid(&error));
    }
}
