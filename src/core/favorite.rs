use crate::core::WallpaperId;

#[derive(Debug, Clone)]
pub struct FavoriteUpdate {
    pub id: WallpaperId,
    pub favorite: bool,
}
