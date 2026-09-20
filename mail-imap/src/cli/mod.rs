use crate::config::Config;
use crate::imap::ImapClient;
use anyhow::Result;

pub async fn list_folders(config: &Config) -> Result<()> {
    let client = ImapClient::connect(config).await?;
    let folders = client.list_folders().await?;
    
    println!("Folders:");
    for folder in folders {
        println!("  - {}", folder);
    }
    
    Ok(())
}

pub async fn search_emails(config: &Config, query: &str) -> Result<()> {
    let client = ImapClient::connect(config).await?;
    let emails = client.search_emails(query).await?;
    
    println!("Found {} emails:", emails.len());
    for email_id in emails {
        println!("  - {}", email_id);
    }
    
    Ok(())
}

pub async fn read_email(config: &Config, id: u32) -> Result<()> {
    let client = ImapClient::connect(config).await?;
    let content = client.get_email(id).await?;
    
    println!("Email {} content:", id);
    println!("{}", content);
    
    Ok(())
}

pub async fn move_email(config: &Config, id: u32, folder: &str) -> Result<()> {
    let client = ImapClient::connect(config).await?;
    client.move_email(id, folder).await?;
    
    println!("Email {} moved to folder '{}'", id, folder);
    
    Ok(())
}

pub async fn tag_email(config: &Config, id: u32, tags: &[String]) -> Result<()> {
    let client = ImapClient::connect(config).await?;
    client.tag_email(id, tags).await?;
    
    println!("Email {} tagged with: {:?}", id, tags);
    
    Ok(())
}