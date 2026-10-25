/// TLS mode is a property of the URL scheme, not a discovered capability.
/// Rustls is the only encrypted backend; successful domains need no cache.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TlsType {
    Plain,
    Rustls,
}

impl TlsType {
    pub fn for_url(value: &str) -> crate::ResultType<Self> {
        if value.len() > 4096 {
            return Err(anyhow::anyhow!("Invalid TLS URL"));
        }
        let url = url::Url::parse(value).map_err(|_| anyhow::anyhow!("Invalid TLS URL"))?;
        if url.host_str().is_none() {
            return Err(anyhow::anyhow!("Invalid TLS URL"));
        }
        match url.scheme() {
            "http" | "ws" => Ok(Self::Plain),
            "https" | "wss" => Ok(Self::Rustls),
            _ => Err(anyhow::anyhow!("Unsupported TLS URL scheme")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_mode_is_deterministic_for_scheme_not_a_previous_domain_result() {
        for value in [
            "http://example.test",
            "ws://example.test/path",
            "HTTP://EXAMPLE.TEST:8080",
        ] {
            assert_eq!(TlsType::for_url(value).unwrap(), TlsType::Plain);
        }
        for value in [
            "https://example.test",
            "wss://[::1]:8443/path",
            "HTTPS://EXAMPLE.TEST",
        ] {
            assert_eq!(TlsType::for_url(value).unwrap(), TlsType::Rustls);
        }
        for index in 0..1000 {
            assert_eq!(
                TlsType::for_url(&format!("https://host-{index}.invalid/path")).unwrap(),
                TlsType::Rustls
            );
        }
        // There is intentionally no global state, admission lock or domain
        // allocation retained by a successful connection.
    }

    #[test]
    fn invalid_urls_fail_with_static_diagnostics() {
        for value in [
            "",
            "file:///private",
            "ftp://user:SECRET@host.test",
            "https://",
            "invalid",
        ] {
            let error = TlsType::for_url(value).unwrap_err().to_string();
            assert!(!error.contains("SECRET"));
            assert!(!error.contains("private"));
        }
        assert!(TlsType::for_url(&"x".repeat(4097)).is_err());
    }
}
