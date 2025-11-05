
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PackageStatus {
    Installed,
    PendingApproval,
    Denied,
    NotInstalled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    pub name: String,
    pub version: Option<String>,
    pub status: PackageStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ApprovalPolicy {
    AllowList,
    AutoApprove,
    RequireApproval,
    DenyAll,
}

pub struct PackageManager {
    policy: ApprovalPolicy,
    allowlist: Vec<String>,
    installed: HashMap<String, String>, // name -> version
    pending: HashMap<String, String>, // name -> reason
}

impl PackageManager {
    pub fn new(policy: ApprovalPolicy) -> Self {
        Self {
            policy,
            allowlist: Vec::new(),
            installed: HashMap::new(),
            pending: HashMap::new(),
        }
    }

    pub fn default() -> Self {
        Self::new(ApprovalPolicy::RequireApproval)
    }

    pub fn add_to_allowlist(&mut self, package: String) {
        if !self.allowlist.contains(&package) {
            self.allowlist.push(package);
        }
    }

    pub fn set_policy(&mut self, policy: ApprovalPolicy) {
        self.policy = policy;
    }

    pub fn policy(&self) -> ApprovalPolicy {
        self.policy
    }

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

    pub fn request_package(&mut self, package: String, reason: Option<String>) -> PackageStatus {
        if self.installed.contains_key(&package) {
            return PackageStatus::Installed;
        }

        match self.policy {
            ApprovalPolicy::DenyAll => PackageStatus::Denied,

            ApprovalPolicy::AutoApprove => {
                self.pending.insert(package.clone(), reason.unwrap_or_default());
                PackageStatus::PendingApproval
            }

            ApprovalPolicy::AllowList => {
                if self.allowlist.contains(&package) {
                    self.pending.insert(package.clone(), reason.unwrap_or_default());
                    PackageStatus::PendingApproval
                } else {
                    PackageStatus::Denied
                }
            }

            ApprovalPolicy::RequireApproval => {
                self.pending.insert(package.clone(), reason.unwrap_or_default());
                PackageStatus::PendingApproval
            }
        }
    }

    pub fn get_status(&self, package: &str) -> PackageStatus {
        if self.installed.contains_key(package) {
            PackageStatus::Installed
        } else if self.pending.contains_key(package) {
            PackageStatus::PendingApproval
        } else {
            PackageStatus::NotInstalled
        }
    }

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

    pub fn approve_package(&mut self, package: &str) -> bool {
        if self.pending.remove(package).is_some() {
            // TODO: Actual installation via PyO3
            self.installed.insert(package.to_string(), "pending".to_string());
            true
        } else {
            false
        }
    }

    pub fn deny_package(&mut self, package: &str) -> bool {
        self.pending.remove(package).is_some()
    }

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

        assert!(manager.approve_package("numpy"));
        assert_eq!(manager.get_status("numpy"), PackageStatus::Installed);
    }

    #[test]
    fn test_deny_pending() {
        let mut manager = PackageManager::new(ApprovalPolicy::RequireApproval);
        manager.request_package("numpy".to_string(), None);
        assert_eq!(manager.get_status("numpy"), PackageStatus::PendingApproval);

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
