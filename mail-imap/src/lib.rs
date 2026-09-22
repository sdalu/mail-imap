pub mod cli;
pub mod config;
pub mod imap;

#[cfg(test)]
mod tests {
    use crate::config::{AccessLevel, Config};
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
        assert!(client.is_mock(), "expected the mock backend");
    }

    fn client_at(level: AccessLevel) -> ImapClient {
        let config = Config {
            access: level,
            ..mock_config()
        };
        ImapClient::connect(&config, false).expect("connect")
    }

    #[test]
    fn readonly_refuses_every_flag_change() {
        let mut client = client_at(AccessLevel::ReadOnly);
        let seen = vec!["\\Seen".to_string()];
        let err = client
            .store_flags("INBOX", &[1], &seen, &[])
            .expect_err("readonly must change nothing");
        assert!(err.to_string().contains("readonly"), "{}", err);
        // Clearing is a change too.
        assert!(client.store_flags("INBOX", &[1], &[], &seen).is_err());
        // ... but reading is not.
        assert!(client.get_email("INBOX", 1).is_ok());
        assert!(client.message_flags("INBOX", 1).is_ok());
    }

    #[test]
    fn organize_sets_anything_but_deleted_and_clears_everything() {
        let mut client = client_at(AccessLevel::Organize);
        let deleted = vec!["\\Deleted".to_string()];
        let seen = vec!["\\Seen".to_string()];
        let junk = vec!["junk".to_string()];
        assert!(client.store_flags("INBOX", &[1], &seen, &[]).is_ok());
        assert!(client.store_flags("INBOX", &[1], &junk, &[]).is_ok());
        let err = client
            .store_flags("INBOX", &[1], &deleted, &[])
            .expect_err("\\Deleted marks a message for removal");
        assert!(err.to_string().contains("Deleted"), "{}", err);
        // Clearing \\Deleted rescues a message, so it is allowed here.
        assert!(client.store_flags("INBOX", &[1], &[], &deleted).is_ok());
    }

    #[test]
    fn organize_leaves_the_folder_tree_alone() {
        let mut client = client_at(AccessLevel::Organize);
        for err in [
            client.create_folder("Archive", None).err(),
            client.rename_folder("Spam", "Junk").err(),
            client.set_subscribed("Drafts", true).err(),
            client.set_subscribed("Drafts", false).err(),
        ] {
            let err = err.expect("organize must not touch the tree");
            assert!(err.to_string().contains("folder tree"), "{}", err);
        }
        // ... while the message-level changes it is for still work.
        assert!(client
            .store_flags("INBOX", &[1], &["\\Seen".to_string()], &[])
            .is_ok());
    }

    #[test]
    fn restructure_changes_the_tree_and_keeps_every_message() {
        let mut client = client_at(AccessLevel::Restructure);
        assert!(client.create_folder("Archive", None).is_ok());
        assert!(
            client.create_folder("Archive", None).is_err(),
            "a mailbox is not created twice"
        );
        assert!(client.rename_folder("Spam", "Junk").is_ok());
        assert!(client.rename_folder("Nowhere", "Somewhere").is_err());
        assert!(client.set_subscribed("Drafts", true).is_ok());
        // The rung is about the tree, not about losing mail.
        assert!(client
            .store_flags("INBOX", &[1], &["\\Deleted".to_string()], &[])
            .is_err());
    }

    #[test]
    fn renaming_inbox_is_refused_at_every_level() {
        // RFC 3501 §6.3.5 makes it move every message out and leave
        // INBOX empty, which nobody means by "rename".
        for level in [AccessLevel::Restructure, AccessLevel::Full] {
            let mut client = client_at(level);
            let err = client
                .rename_folder("INBOX", "Old")
                .expect_err("INBOX is refused outright");
            assert!(err.to_string().contains("empty"), "{}", err);
            assert!(
                client.rename_folder("inbox", "Old").is_err(),
                "case-insensitively, as IMAP defines INBOX"
            );
        }
    }

    #[test]
    fn a_special_use_attribute_needs_the_server_to_take_one() {
        let mut client = client_at(AccessLevel::Restructure);
        let err = client
            .create_folder("Archive", Some("\\Archive"))
            .expect_err("the mock advertises no capabilities");
        assert!(err.to_string().contains("CREATE-SPECIAL-USE"), "{}", err);
    }

    #[test]
    fn readonly_refuses_the_tree_too() {
        let mut client = client_at(AccessLevel::ReadOnly);
        assert!(client.create_folder("Archive", None).is_err());
        assert!(client.rename_folder("Spam", "Junk").is_err());
        assert!(client.set_subscribed("Drafts", true).is_err());
    }

    #[test]
    fn full_permits_what_organize_refuses() {
        let mut client = client_at(AccessLevel::Full);
        let deleted = vec!["\\Deleted".to_string()];
        assert!(client.store_flags("INBOX", &[1], &deleted, &[]).is_ok());
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
            .search_folders(&folders, "", 50, None)
            .expect("search");
        assert!(!results.is_empty());
        assert!(results.iter().all(|r| r.folder == "INBOX"));
    }

    #[test]
    fn search_filters_by_subject() {
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        let folders = vec!["INBOX".to_string()];
        let results = client
            .search_folders(&folders, "invoice", 50, None)
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
            .search_folders(&folders, "", 3, None)
            .expect("search");
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| r.folder == "INBOX"));

        let all = client
            .search_folders(&folders, "", 0, None)
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
            .search_folders(&folders, "UNSEEN", 50, None)
            .expect("unread");
        assert!(!results.is_empty());
    }

    #[test]
    fn mock_search_sorted_by_subject() {
        use crate::imap::parse_sort;
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        let folders = vec!["INBOX".to_string()];
        let spec = parse_sort("subject").expect("parse");
        let results = client
            .search_folders(&folders, "", 50, Some(&spec))
            .expect("search");
        let uids: Vec<_> = results.iter().map(|r| r.uid).collect();
        // Lunch? < Meeting notes < Quarterly report < Welcome aboard < Your invoice
        assert_eq!(uids, vec![4, 2, 3, 1, 5]);
    }

    #[test]
    fn mock_search_sorted_descending_uid_respects_cap() {
        use crate::imap::parse_sort;
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        let folders = vec!["INBOX".to_string()];
        let spec = parse_sort("-uid").expect("parse");
        let results = client
            .search_folders(&folders, "", 3, Some(&spec))
            .expect("search");
        let uids: Vec<_> = results.iter().map(|r| r.uid).collect();
        assert_eq!(uids, vec![5, 4, 3]);
    }

    #[test]
    fn mock_search_sort_by_from_descending() {
        use crate::imap::parse_sort;
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        let folders = vec!["INBOX".to_string()];
        let spec = parse_sort("-from").expect("parse");
        let results = client
            .search_folders(&folders, "", 50, Some(&spec))
            .expect("search");
        let uids: Vec<_> = results.iter().map(|r| r.uid).collect();
        // dave > carol > billing > bob ... wait descending: dave(4), carol(3), bob(2), billing(5), alice(1)
        assert_eq!(uids, vec![4, 3, 2, 5, 1]);
    }

    #[test]
    fn mock_flag_add_remove_roundtrip() {
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        let flags = vec!["\\Seen".to_string(), "junk".to_string()];
        client
            .store_flags("INBOX", &[2, 3], &flags, &[])
            .expect("add flags");
        let folders = vec!["INBOX".to_string()];
        let results = client
            .search_folders(&folders, "", 50, None)
            .expect("search");
        let m2 = results.iter().find(|r| r.uid == 2).expect("uid 2");
        assert_eq!(m2.flags, vec!["\\Seen".to_string(), "junk".to_string()]);
        let m1 = results.iter().find(|r| r.uid == 1).expect("uid 1");
        assert!(m1.flags.is_empty());

        client
            .store_flags("INBOX", &[2], &[], &flags)
            .expect("remove flags");
        let results = client
            .search_folders(&folders, "", 50, None)
            .expect("search");
        let m2 = results.iter().find(|r| r.uid == 2).expect("uid 2");
        assert!(m2.flags.is_empty());
        let m3 = results.iter().find(|r| r.uid == 3).expect("uid 3");
        assert_eq!(m3.flags.len(), 2);
    }

    #[test]
    fn mock_flag_unknown_uid_fails() {
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        assert!(client
            .store_flags("INBOX", &[999], &["\\Seen".to_string()], &[])
            .is_err());
    }

    #[test]
    fn mock_message_flags_lists_stored_flags() {
        let mut client = ImapClient::connect(&mock_config(), false).expect("connect");
        assert!(client
            .message_flags("INBOX", 1)
            .expect("list")
            .is_empty());
        let flags = vec!["\\Seen".to_string(), "invoice".to_string()];
        client
            .store_flags("INBOX", &[1], &flags, &[])
            .expect("add");
        assert_eq!(
            client.message_flags("INBOX", 1).expect("list"),
            vec!["\\Seen".to_string(), "invoice".to_string()]
        );
        assert!(client.message_flags("INBOX", 999).is_err());
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
