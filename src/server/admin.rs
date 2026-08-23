use super::EnhancedGameServer;

impl EnhancedGameServer {
    pub async fn health_check(&self) -> bool {
        self.database.health_check().await
    }
}
