//! Role definitions and helpers for the assistant.
//!
//! Two roles are supported: `Owner` (Alex, full access) and `Operator` (employees,
//! read-only tools only). The tool registry uses this to filter available tools at
//! session start.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A user role that governs which tools are available in a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The business owner (Alex). Has access to all tools including Write and Confirm.
    Owner,
    /// An employee. Read-only tools only.
    Operator,
}

impl Role {
    /// Returns true if this role is permitted to execute tools flagged as Owner-only.
    pub fn is_owner(self) -> bool {
        matches!(self, Role::Owner)
    }

    /// Returns true if this role can execute a tool that requires the given minimum role.
    pub fn satisfies(self, required: Role) -> bool {
        match required {
            Role::Owner => self == Role::Owner,
            Role::Operator => true, // Both roles satisfy Operator-level.
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Role::Owner => write!(f, "owner"),
            Role::Operator => write!(f, "operator"),
        }
    }
}

impl TryFrom<&str> for Role {
    type Error = crate::error::AssistantError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "owner" => Ok(Role::Owner),
            "operator" => Ok(Role::Operator),
            other => Err(crate::error::AssistantError::Internal(format!(
                "Unknown role: '{other}'"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_satisfies_owner() {
        assert!(Role::Owner.satisfies(Role::Owner));
    }

    #[test]
    fn owner_satisfies_operator() {
        assert!(Role::Owner.satisfies(Role::Operator));
    }

    #[test]
    fn operator_does_not_satisfy_owner() {
        assert!(!Role::Operator.satisfies(Role::Owner));
    }

    #[test]
    fn operator_satisfies_operator() {
        assert!(Role::Operator.satisfies(Role::Operator));
    }

    #[test]
    fn parse_role_from_str() {
        assert_eq!(Role::try_from("owner").unwrap(), Role::Owner);
        assert_eq!(Role::try_from("operator").unwrap(), Role::Operator);
        assert!(Role::try_from("admin").is_err());
    }
}
