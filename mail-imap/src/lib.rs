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
        let client = ImapClient::connect(&mock_config(), false).expect("connect");
        match client {
            ImapClient::Mock(_) => {}
            ImapClient::Real(_) => panic!("expected mock backend, got real"),
        }
    }

    #[test]
    fn list_folders_returns_expected_set() {
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        let folders = client.list_folders().expect("list");
        let names: Vec<_> = folders.iter().map(|f| f.name.clone()).collect();
        assert!(names.contains(&"INBOX".to_string()));
        assert!(names.contains(&"Sent Items".to_string()));
        assert!(names.contains(&"Trash".to_string()));
        assert!(!names.is_empty());
    }

    #[test]
    fn search_returns_all_when_blank() {
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        let folders = vec!["INBOX".to_string()];
        let results = client
            .search_folders(&folders, "", 50)
            .expect("search");
        assert!(!results.is_empty());
        assert!(results.iter().all(|r| r.folder == "INBOX"));
    }

    #[test]
    fn search_filters_by_subject() {
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        let folders = vec!["INBOX".to_string()];
        let results = client
            .search_folders(&folders, "invoice", 50)
            .expect("search");
        assert!(results.iter().any(|r| r.subject == "Your invoice"));
        assert!(!results.iter().any(|r| r.subject == "Lunch?"));
    }

    #[test]
    fn search_multi_folder_aggregates_and_caps_total() {
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        let folders = vec!["INBOX".to_string(), "Trash".to_string()];
        // The mock reports its 5 messages for every folder; with a total
        // budget of 3 the first folder fills it and the second is skipped.
        let results = client
            .search_folders(&folders, "", 3)
            .expect("search");
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| r.folder == "INBOX"));

        let all = client
            .search_folders(&folders, "", 0)
            .expect("search");
        assert_eq!(all.len(), 10);
        let in_trash: Vec<_> = all.iter().filter(|r| r.folder == "Trash").collect();
        assert_eq!(in_trash.len(), 5);
    }

    #[test]
    fn read_known_uid() {
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        let content = client.get_email("INBOX", 1).expect("read");
        assert!(content.contains("Welcome aboard"));
        assert!(content.contains("Hello, this is message 1."));
    }

    #[test]
    fn read_unknown_uid_fails() {
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        assert!(client.get_email("INBOX", 999).is_err());
    }

    #[test]
    fn mailbox_counts_all_folders() {
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        let counts = client.mailbox_counts(None).expect("counts");
        assert!(counts.iter().any(|m| m.name == "INBOX" && m.messages == 5));
        assert!(counts.iter().any(|m| m.name == "Trash" && m.messages == 0));
    }

    #[test]
    fn mailbox_counts_single_folder() {
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        let counts = client.mailbox_counts(Some("INBOX")).expect("counts");
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[0].name, "INBOX");
        assert_eq!(counts[0].uid_next, 6);
        assert!(client.mailbox_counts(Some("Nope")).is_err());
    }

    #[test]
    fn folder_uids_returns_all_uids() {
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        let uids = client.folder_uids("INBOX").expect("uids");
        assert_eq!(uids, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn unread_search_returns_unseen_messages() {
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        let folders = vec!["INBOX".to_string()];
        let results = client
            .search_folders(&folders, "UNSEEN", 50)
            .expect("unread");
        assert!(!results.is_empty());
    }

    #[test]
    fn list_parts_known_uid() {
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        let parts = client.list_parts("INBOX", 5).expect("parts");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].part, 1);
        assert_eq!(parts[1].part, 2);
        assert_eq!(parts[1].filename.as_deref(), Some("invoice-42.pdf"));
        assert_eq!(parts[1].content_type, "application/pdf");
    }

    #[test]
    fn list_parts_unknown_uid_fails() {
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        assert!(client.list_parts("INBOX", 999).is_err());
    }

    #[test]
    fn save_part_writes_file() {
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        let dir = std::env::temp_dir();
        let path = dir.join(format!("mail-imap-test-part-{}.pdf", std::process::id()));
        let size = client
            .save_part("INBOX", 5, 2, &path)
            .expect("save");
        assert!(size > 0);
        let data = std::fs::read_to_string(&path).expect("read back");
        assert!(data.contains("uid 5 part 2"));
        let _ = std::fs::remove_file(&path);
        assert!(client.save_part("INBOX", 5, 99, &path).is_err());
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
        assert!(ImapClient::connect(&cfg, false).is_err());
    }
}
