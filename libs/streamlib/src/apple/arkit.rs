


#[cfg(target_os = "ios")]
pub mod ios {
    use super::*;
}

#[cfg(target_os = "macos")]
pub mod macos {
    
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_arkit_availability() {
    }
}
