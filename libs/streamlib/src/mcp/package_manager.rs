//! Package Manager
//!
//! Manages package installations for dynamic processors across multiple languages.
//! Implements security policies for package approval.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Package installation status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PackageStatus {
    /// Package is installed and ready to use
    Installed,
    /// Package installation is pending approval
    PendingApproval,
    /// Package installation was denied by policy
    Denied,
    /// Package is not installed
    NotInstalled,
}

/// Information about a package
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    /// Package name (e.g., "scikit-learn", "pillow")
    pub name: String,
    /// Installed version (if installed)
    pub version: Option<String>,
    /// Installation status
    pub status: PackageStatus,
    /// Reason for request (if applicable)
    pub reason: Option<String>,
}

/// Package approval policy
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ApprovalPolicy {
    /// Only packages in the allowlist can be installed
    AllowList,
    /// Automatically approve all package requests
    AutoApprove,
    /// Require manual approval for all packages
    RequireApproval,
    /// Deny all package installation requests
    DenyAll,
}

/// Package manager
///
/// Tracks installed packages and manages installation requests.
/// Language-agnostic - can be used for Python, TypeScript, or other languages.
pub struct PackageManager {
    /// Current approval policy
    policy: ApprovalPolicy,
    /// Allowlist of approved packages (used with AllowList policy)
    allowlist: Vec<String>,
    /// Installed packages
    installed: HashMap<String, String>, // name -> version
    /// Pending approval requests
    pending: HashMap<String, String>, // name -> reason
}

impl PackageManager {
    /// Create a new package manager with a policy
    pub fn new(policy: ApprovalPolicy) -> Self {
        Self {
            policy,
            allowlist: Vec::new(),
            installed: HashMap::new(),
            pending: HashMap::new(),
        }
    }

    /// Create with default policy (RequireApproval)
    pub fn default() -> Self {
        Self::new(ApprovalPolicy::RequireApproval)
    }

    /// Add a package to the allowlist
    pub fn add_to_allowlist(&mut self, package: String) {
        if !self.allowlist.contains(&package) {
            self.allowlist.push(package);
        }
    }

    /// Set the approval policy
    pub fn set_policy(&mut self, policy: ApprovalPolicy) {
        self.policy = policy;
    }

    /// Get current policy
    pub fn policy(&self) -> ApprovalPolicy {
        self.policy
    }

    /// List all installed packages
    pub fn list_installed(&self) -> Vec<PackageInfo> {
        self.installed
            .iter()
            .map(|(name, version)| PackageInfo {
                name: name.clone(),
                version: Some(version.clone()),
                status: PackageStatus::Installed,
                reason: None,
            })
            .collect()
    }

    /// Request a package installation
    ///
    /// Returns the status after evaluating the request against the policy.
    pub fn request_package(&mut self, package: String, reason: Option<String>) -> PackageStatus {
        // Check if already installed
        if self.installed.contains_key(&package) {
            return PackageStatus::Installed;
        }

        // Evaluate against policy
        match self.policy {
            ApprovalPolicy::DenyAll => PackageStatus::Denied,

            ApprovalPolicy::AutoApprove => {
                // Auto-approve (actual installation would happen here)
                // For now, just mark as pending since we don't have PyO3 yet
                self.pending.insert(package.clone(), reason.unwrap_or_default());
                PackageStatus::PendingApproval
            }

            ApprovalPolicy::AllowList => {
                if self.allowlist.contains(&package) {
                    // Approved via allowlist
                    self.pending.insert(package.clone(), reason.unwrap_or_default());
                    PackageStatus::PendingApproval
                } else {
                    PackageStatus::Denied
                }
            }

            ApprovalPolicy::RequireApproval => {
                // Require manual approval
                self.pending.insert(package.clone(), reason.unwrap_or_default());
                PackageStatus::PendingApproval
            }
        }
    }

    /// Get the status of a package
    pub fn get_status(&self, package: &str) -> PackageStatus {
        if self.installed.contains_key(package) {
            PackageStatus::Installed
        } else if self.pending.contains_key(package) {
            PackageStatus::PendingApproval
        } else {
            PackageStatus::NotInstalled
        }
    }

    /// Get information about a package
    pub fn get_package_info(&self, package: &str) -> PackageInfo {
        let status = self.get_status(package);
        let version = self.installed.get(package).cloned();
        let reason = self.pending.get(package).cloned();

        PackageInfo {
            name: package.to_string(),
            version,
            status,
            reason,
        }
    }

    /// Approve a pending package (for manual approval)
    ///
    /// This would trigger actual installation via PyO3.
    pub fn approve_package(&mut self, package: &str) -> bool {
        if self.pending.remove(package).is_some() {
            // TODO: Actual installation via PyO3
            // For now, just mark as installed with placeholder version
            self.installed.insert(package.to_string(), "pending".to_string());
            true
        } else {
            false
        }
    }

    /// Deny a pending package
    pub fn deny_package(&mut self, package: &str) -> bool {
        self.pending.remove(package).is_some()
    }

    /// List packages pending approval
    pub fn list_pending(&self) -> Vec<PackageInfo> {
        self.pending
            .iter()
            .map(|(name, reason)| PackageInfo {
                name: name.clone(),
                version: None,
                status: PackageStatus::PendingApproval,
                reason: Some(reason.clone()),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_approve_policy() {
        let mut manager = PackageManager::new(ApprovalPolicy::AutoApprove);
        let status = manager.request_package("numpy".to_string(), Some("for array ops".to_string()));
        assert_eq!(status, PackageStatus::PendingApproval);
    }

    #[test]
    fn test_deny_all_policy() {
        let mut manager = PackageManager::new(ApprovalPolicy::DenyAll);
        let status = manager.request_package("numpy".to_string(), None);
        assert_eq!(status, PackageStatus::Denied);
    }

    #[test]
    fn test_allowlist_policy() {
        let mut manager = PackageManager::new(ApprovalPolicy::AllowList);
        manager.add_to_allowlist("numpy".to_string());

        let status = manager.request_package("numpy".to_string(), None);
        assert_eq!(status, PackageStatus::PendingApproval);

        let status = manager.request_package("unknown-package".to_string(), None);
        assert_eq!(status, PackageStatus::Denied);
    }

    #[test]
    fn test_require_approval_policy() {
        let mut manager = PackageManager::new(ApprovalPolicy::RequireApproval);
        let status = manager.request_package("numpy".to_string(), None);
        assert_eq!(status, PackageStatus::PendingApproval);

        // Approve it
        assert!(manager.approve_package("numpy"));
        assert_eq!(manager.get_status("numpy"), PackageStatus::Installed);
    }

    #[test]
    fn test_deny_pending() {
        let mut manager = PackageManager::new(ApprovalPolicy::RequireApproval);
        manager.request_package("numpy".to_string(), None);
        assert_eq!(manager.get_status("numpy"), PackageStatus::PendingApproval);

        // Deny it
        assert!(manager.deny_package("numpy"));
        assert_eq!(manager.get_status("numpy"), PackageStatus::NotInstalled);
    }

    #[test]
    fn test_already_installed() {
        let mut manager = PackageManager::new(ApprovalPolicy::RequireApproval);
        manager.installed.insert("numpy".to_string(), "1.24.0".to_string());

        let status = manager.request_package("numpy".to_string(), None);
        assert_eq!(status, PackageStatus::Installed);
    }

    #[test]
    fn test_list_packages() {
        let mut manager = PackageManager::new(ApprovalPolicy::RequireApproval);
        manager.installed.insert("numpy".to_string(), "1.24.0".to_string());
        manager.request_package("opencv-python".to_string(), Some("for image processing".to_string()));

        let installed = manager.list_installed();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].name, "numpy");

        let pending = manager.list_pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].name, "opencv-python");
        assert_eq!(pending[0].reason, Some("for image processing".to_string()));
    }
}
