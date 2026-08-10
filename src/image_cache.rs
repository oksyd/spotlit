use std::{cell::RefCell, collections::VecDeque};

use slint::Image;

const DISPLAY_PREVIEW_CACHE_LIMIT: usize = 2;
pub(crate) const LIST_THUMBNAIL_CACHE_LIMIT: usize = 24;

thread_local! {
    static DISPLAY_PREVIEWS: RefCell<ImageCache> =
        RefCell::new(ImageCache::new(DISPLAY_PREVIEW_CACHE_LIMIT));
    static LIST_THUMBNAILS: RefCell<ImageCache> =
        RefCell::new(ImageCache::new(LIST_THUMBNAIL_CACHE_LIMIT));
}

pub(crate) fn remember_display_preview(id: &str, preview_path: &str, image: &Image) {
    DISPLAY_PREVIEWS.with_borrow_mut(|cache| cache.remember(id, preview_path, image));
}

pub(crate) fn display_preview(id: &str, preview_path: &str) -> Option<Image> {
    DISPLAY_PREVIEWS.with_borrow_mut(|cache| cache.get(id, preview_path))
}

pub(crate) fn remember_list_thumbnail(id: &str, preview_path: &str, image: &Image) {
    LIST_THUMBNAILS.with_borrow_mut(|cache| cache.remember(id, preview_path, image));
}

pub(crate) fn list_thumbnail(id: &str, preview_path: &str) -> Option<Image> {
    LIST_THUMBNAILS.with_borrow_mut(|cache| cache.get(id, preview_path))
}

pub(crate) fn list_thumbnail_by_path(preview_path: &str) -> Option<Image> {
    LIST_THUMBNAILS.with_borrow_mut(|cache| cache.get_by_preview_path(preview_path))
}

pub(crate) fn release_decoded_images() {
    DISPLAY_PREVIEWS.with_borrow_mut(ImageCache::clear);
    LIST_THUMBNAILS.with_borrow_mut(ImageCache::clear);
}

struct ImageCache {
    limit: usize,
    images: VecDeque<CachedImage>,
}

impl ImageCache {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            images: VecDeque::new(),
        }
    }

    fn remember(&mut self, id: &str, preview_path: &str, image: &Image) {
        if id.is_empty() || preview_path.is_empty() || !image_has_pixels(image) || self.limit == 0 {
            return;
        }

        self.remove(id, preview_path);
        self.images.push_back(CachedImage {
            id: id.to_string(),
            preview_path: preview_path.to_string(),
            image: image.clone(),
        });

        while self.images.len() > self.limit {
            self.images.pop_front();
        }
    }

    fn get(&mut self, id: &str, preview_path: &str) -> Option<Image> {
        let position = self
            .images
            .iter()
            .position(|image| image.id == id && image.preview_path == preview_path)?;
        let image = self.images.remove(position)?;
        let result = image.image.clone();
        self.images.push_back(image);
        Some(result)
    }

    fn get_by_preview_path(&mut self, preview_path: &str) -> Option<Image> {
        if preview_path.is_empty() {
            return None;
        }

        let position = self
            .images
            .iter()
            .position(|image| image.preview_path == preview_path)?;
        let image = self.images.remove(position)?;
        let result = image.image.clone();
        self.images.push_back(image);
        Some(result)
    }

    fn remove(&mut self, id: &str, preview_path: &str) {
        if let Some(position) = self
            .images
            .iter()
            .position(|image| image.id == id && image.preview_path == preview_path)
        {
            self.images.remove(position);
        }
    }

    fn clear(&mut self) {
        self.images.clear();
    }
}

struct CachedImage {
    id: String,
    preview_path: String,
    image: Image,
}

fn image_has_pixels(image: &Image) -> bool {
    let size = image.size();
    size.width > 0 && size.height > 0
}

#[cfg(test)]
mod tests {
    use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

    use super::{ImageCache, release_decoded_images};

    #[test]
    fn cache_returns_images_by_id_and_preview_path() {
        let mut cache = ImageCache::new(2);
        let image = test_image();

        cache.remember("first", "first.jpg", &image);

        assert!(cache.get("first", "first.jpg").is_some());
        assert!(cache.get("first", "other.jpg").is_none());
        assert!(cache.get("other", "first.jpg").is_none());
    }

    #[test]
    fn cache_evicts_least_recently_used_image() {
        let mut cache = ImageCache::new(2);
        let image = test_image();

        cache.remember("first", "first.jpg", &image);
        cache.remember("second", "second.jpg", &image);
        assert!(cache.get("first", "first.jpg").is_some());
        cache.remember("third", "third.jpg", &image);

        assert!(cache.get("first", "first.jpg").is_some());
        assert!(cache.get("second", "second.jpg").is_none());
        assert!(cache.get("third", "third.jpg").is_some());
    }

    #[test]
    fn cache_can_reuse_image_by_preview_path() {
        let mut cache = ImageCache::new(2);
        let image = test_image();

        cache.remember("first-id", "shared.jpg", &image);

        assert!(cache.get_by_preview_path("shared.jpg").is_some());
        assert!(cache.get_by_preview_path("missing.jpg").is_none());
    }

    #[test]
    fn cache_can_be_cleared() {
        let mut cache = ImageCache::new(2);
        let image = test_image();

        cache.remember("first", "first.jpg", &image);
        cache.clear();

        assert!(cache.get("first", "first.jpg").is_none());
    }

    #[test]
    fn decoded_image_release_clears_thread_local_caches() {
        let image = test_image();

        super::remember_display_preview("display", "display.jpg", &image);
        super::remember_list_thumbnail("thumb", "thumb.jpg", &image);

        release_decoded_images();

        assert!(super::display_preview("display", "display.jpg").is_none());
        assert!(super::list_thumbnail("thumb", "thumb.jpg").is_none());
    }

    fn test_image() -> Image {
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(1, 1);
        buffer
            .make_mut_bytes()
            .copy_from_slice(&[255, 255, 255, 255]);
        Image::from_rgba8(buffer)
    }
}
