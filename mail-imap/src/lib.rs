pub mod cli;
pub mod config;
pub mod imap;

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::imap::{ImapBackend, ImapClient};

    fn mock_config() -> Config {
        Config {
            mock: true,
            ..Config::default()
        }
    }

    #[test]
    fn connect_chooses_mock_backend() {
        let client = ImapClient::connect(&mock_config()).expect("connect");
        match client {
            ImapClient::Mock(_) => {}
            ImapClient::Real(_) => panic!("expected mock backend, got real"),
        }
    }

    #[test]
    fn list_folders_returns_expected_set() {
        let mut client = ImapClient::connect(&mock_config()).expect("connect");
        let folders = client.list_folders().expect("list");
        let names: Vec<_> = folders.iter().map(|f| f.name.clone()).collect();
        assert!(names.contains(&"INBOX".to_string()));
        assert!(names.contains(&"Sent Items".to_string()));
        assert!(names.contains(&"Trash".to_string()));
        assert!(!names.is_empty());
    }

    #[test]
    fn search_returns_all_when_blank() {
        let mut client = ImapClient::connect(&mock_config()).expect("connect");
        let results = client.search_emails("INBOX", "").expect("search");
        assert!(!results.is_empty());
    }

    #[test]
    fn search_filters_by_subject() {
        let mut client = ImapClient::connect(&mock_config()).expect("connect");
        let results = client
            .search_emails("INBOX", "invoice")
            .expect("search");
        assert!(results.iter().any(|r| r.subject == "Your invoice"));
        assert!(!results.iter().any(|r| r.subject == "Lunch?"));
    }

    #[test]
    fn read_known_uid() {
        let mut client = ImapClient::connect(&mock_config()).expect("connect");
        let content = client.get_email("INBOX", 1).expect("read");
        assert!(content.contains("Welcome aboard"));
        assert!(content.contains("Hello, this is message 1."));
    }

    #[test]
    fn read_unknown_uid_fails() {
        let mut client = ImapClient::connect(&mock_config()).expect("connect");
        assert!(client.get_email("INBOX", 999).is_err());
    }

    #[test]
    fn move_known_uid_succeeds() {
        let mut client = ImapClient::connect(&mock_config()).expect("connect");
        assert!(client.move_email("INBOX", 1, "Trash").is_ok());
        assert!(client.move_email("INBOX", 999, "Trash").is_err());
    }

    #[test]
    fn tag_known_uid_succeeds() {
        let mut client = ImapClient::connect(&mock_config()).expect("connect");
        assert!(client
            .tag_email("INBOX", 1, &["important".to_string()])
            .is_ok());
        assert!(client.tag_email("INBOX", 999, &["x".to_string()]).is_err());
    }

    #[test]
    fn real_backend_refuses_to_fabricate_when_unreachable() {
        // Point the real backend at a closed local port: it must fail cleanly
        // (no fabricated mock data) instead of pretending to succeed.
        let cfg = Config {
            server: "127.0.0.1".to_string(),
            port: 1,
            mock: false,
            ..Config::default()
        };
        assert!(ImapClient::connect(&cfg).is_err());
    }
}
