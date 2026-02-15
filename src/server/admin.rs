use super::EnhancedGameServer;
use anyhow::Result;

impl EnhancedGameServer {
    pub async fn admin_user_exists(&self, email: &str) -> Result<bool> {
        self.database.admin_user_exists(email).await
    }

    pub async fn health_check(&self) -> bool {
        self.database.health_check().await
    }
}
