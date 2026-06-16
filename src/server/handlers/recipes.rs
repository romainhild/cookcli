use crate::{server::AppState, util::PARSER};
use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    Json,
};
use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use cooklang_find;
use image::ImageFormat;
use serde::{Deserialize, Serialize};
use serde_json;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

#[derive(Deserialize)]
pub struct RecipeQuery {
    scale: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    q: String,
}

fn json_error(msg: impl std::fmt::Display) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "error": msg.to_string() }))
}

fn check_path(p: &str) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let path = Utf8Path::new(p);
    if !path
        .components()
        .all(|c| matches!(c, Utf8Component::Normal(_)))
    {
        tracing::error!("Invalid path: {p}");
        return Err((
            StatusCode::BAD_REQUEST,
            json_error(format!("Invalid path: {p}")),
        ));
    }
    Ok(())
}

pub async fn all_recipes(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let recipes = cooklang_find::build_tree(&state.base_path).map_err(|e| {
        tracing::error!("Failed to build recipe tree: {:?}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, json_error(&e))
    })?;

    let recipes = serde_json::to_value(recipes).map_err(|e| {
        tracing::error!("Failed to serialize recipes: {:?}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, json_error(&e))
    })?;

    Ok(Json(recipes))
}

pub async fn recipe(
    Path(path): Path<String>,
    State(state): State<Arc<AppState>>,
    Query(query): Query<RecipeQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    check_path(&path)?;

    let entry = cooklang_find::get_recipe(vec![&state.base_path], &Utf8PathBuf::from(&path))
        .map_err(|e| {
            tracing::error!("Recipe not found: {path}");
            (
                StatusCode::NOT_FOUND,
                json_error(format!("Recipe not found: {path}: {e}")),
            )
        })?;

    let recipe =
        crate::util::parse_recipe_from_entry(&entry, query.scale.unwrap_or(1.0)).map_err(|e| {
            tracing::error!("Failed to parse recipe: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, json_error(&e))
        })?;

    // Get the image path if available
    let image_path = entry.title_image().clone().and_then(|img_path| {
        // If it's a URL, use it directly
        if img_path.starts_with("http://") || img_path.starts_with("https://") {
            Some(img_path)
        } else {
            // For file paths, make them relative and accessible via /api/static
            let img_path = camino::Utf8Path::new(&img_path);

            // Try to strip the base_path prefix to get a relative path
            if let Ok(relative) = img_path.strip_prefix(&state.base_path) {
                Some(format!("/api/static/{relative}"))
            } else {
                // If the path doesn't start with base_path, it might already be relative
                // or it might be an absolute path to a file within base_path
                if !img_path.is_absolute() {
                    Some(format!("/api/static/{img_path}"))
                } else {
                    // Last resort: try to get just the filename
                    img_path
                        .file_name()
                        .map(|name| format!("/api/static/{name}"))
                }
            }
        }
    });

    #[derive(Serialize)]
    struct ApiRecipe {
        #[serde(flatten)]
        recipe: Arc<cooklang::Recipe>,
        grouped_ingredients: Vec<serde_json::Value>,
    }

    let grouped_ingredients = recipe
        .group_ingredients(PARSER.converter())
        .into_iter()
        .map(|entry| {
            serde_json::json!({
                "index": entry.index,
                "quantities": entry.quantity.into_vec()
            })
        })
        .collect();

    let api_recipe = ApiRecipe {
        recipe,
        grouped_ingredients,
    };

    let value = serde_json::json!({
        "recipe": api_recipe,
        "image": image_path,
        "scale": query.scale.unwrap_or(1.0),
        // TODO: add more metadata if needed
    });

    Ok(Json(value))
}

pub async fn recipe_raw(
    Path(path): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<String, (StatusCode, Json<serde_json::Value>)> {
    check_path(&path)?;

    let recipe_path = state.base_path.join(&path);

    // Try .cook extension first, then .menu
    let file_path = if recipe_path.exists() {
        recipe_path
    } else {
        let cook_path = Utf8PathBuf::from(format!("{}.cook", recipe_path));
        let menu_path = Utf8PathBuf::from(format!("{}.menu", recipe_path));

        if cook_path.exists() {
            cook_path
        } else if menu_path.exists() {
            menu_path
        } else {
            tracing::error!("Recipe file not found: {path}");
            return Err((
                StatusCode::NOT_FOUND,
                json_error(format!("Recipe file not found: {path}")),
            ));
        }
    };

    tokio::fs::read_to_string(&file_path).await.map_err(|e| {
        tracing::error!("Failed to read recipe file {}: {}", file_path, e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json_error(format!("Failed to read recipe file: {e}")),
        )
    })
}

pub async fn recipe_save(
    Path(path): Path<String>,
    State(state): State<Arc<AppState>>,
    body: String,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    check_path(&path)?;

    let recipe_path = state.base_path.join(&path);

    // Determine actual file path (with extension)
    let file_path = if recipe_path.exists() {
        recipe_path
    } else {
        let cook_path = Utf8PathBuf::from(format!("{}.cook", recipe_path));
        let menu_path = Utf8PathBuf::from(format!("{}.menu", recipe_path));

        if cook_path.exists() {
            cook_path
        } else if menu_path.exists() {
            menu_path
        } else {
            // Default to .cook for new files
            Utf8PathBuf::from(format!("{}.cook", recipe_path))
        }
    };

    // Atomic write: write to temp file, then rename
    let temp_path = file_path.with_extension("tmp");

    let mut temp_file = tokio::fs::File::create(&temp_path).await.map_err(|e| {
        tracing::error!("Failed to create temp file {}: {}", temp_path, e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json_error(format!(
                "Failed to save recipe: {e}. Check that the recipes folder has write permissions."
            )),
        )
    })?;

    temp_file.write_all(body.as_bytes()).await.map_err(|e| {
        tracing::error!("Failed to write to temp file {}: {}", temp_path, e);
        // Fire-and-forget cleanup - spawn so we don't block the error path
        let temp_path_clone = temp_path.clone();
        tokio::spawn(async move {
            let _ = tokio::fs::remove_file(&temp_path_clone).await;
        });
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json_error(format!("Failed to write recipe: {e}")),
        )
    })?;

    // Replace the original file with the temp file. `rename_replace_async`
    // uses an atomic rename on capable platforms but copies + removes on
    // Android, where the aarch64 `rename()` libc wrapper hits the
    // seccomp-blocked `renameat2` syscall (SIGSYS / "Bad system call").
    // See https://github.com/cooklang/cookcli/issues/349.
    crate::server::fs_atomic::rename_replace_async(temp_path.clone(), file_path.clone())
        .await
        .map_err(|e| {
            tracing::error!("Failed to rename temp file to {}: {}", file_path, e);
            let temp_path_clone = temp_path.clone();
            tokio::spawn(async move {
                let _ = tokio::fs::remove_file(&temp_path_clone).await;
            });
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                json_error(format!("Failed to save recipe: {e}")),
            )
        })?;

    tracing::info!("Saved recipe: {}", file_path);

    Ok(Json(serde_json::json!({
        "status": "success",
        "path": path
    })))
}

pub async fn reload() -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // Since the server reads from disk on each request, there's no cache to clear.
    // This endpoint just returns success to indicate the reload was processed.
    tracing::info!("Reload requested - recipes will be refreshed from disk on next request");
    Ok(Json(serde_json::json!({
        "status": "success",
        "message": "Recipes will be refreshed from disk on next request"
    })))
}

pub async fn search(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<serde_json::Value>)> {
    let recipes = cooklang_find::search(&state.base_path, &query.q).map_err(|e| {
        tracing::error!("Failed to search recipes: {:?}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, json_error(&e))
    })?;

    let results = recipes
        .into_iter()
        .filter_map(|recipe| {
            recipe.path().map(|path| {
                let relative_path = path.strip_prefix(&state.base_path).unwrap_or(path);
                serde_json::json!({
                    "name": recipe.name(),
                    "path": relative_path.to_string()
                })
            })
        })
        .collect();

    Ok(Json(results))
}

pub(crate) fn process_image(bytes: &[u8]) -> Result<(Vec<u8>, &'static str), anyhow::Error> {
    let format = image::guess_format(bytes)
        .map_err(|e| anyhow::anyhow!("Cannot detect image format: {e}"))?;

    match format {
        ImageFormat::Jpeg => Ok((bytes.to_vec(), "jpg")),
        ImageFormat::Png => Ok((bytes.to_vec(), "png")),
        ImageFormat::WebP => Ok((bytes.to_vec(), "webp")),
        _ => {
            let img = image::load_from_memory(bytes)
                .map_err(|e| anyhow::anyhow!("Cannot decode image: {e}"))?;
            let mut buf = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut buf), ImageFormat::Jpeg)
                .map_err(|e| anyhow::anyhow!("Cannot encode image as JPEG: {e}"))?;
            Ok((buf, "jpg"))
        }
    }
}

pub async fn recipe_delete(
    Path(path): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    check_path(&path)?;

    let recipe_path = state.base_path.join(&path);

    // Determine actual file path (with extension)
    let file_path = if recipe_path.exists() {
        recipe_path
    } else {
        let cook_path = Utf8PathBuf::from(format!("{}.cook", recipe_path));
        let menu_path = Utf8PathBuf::from(format!("{}.menu", recipe_path));

        if cook_path.exists() {
            cook_path
        } else if menu_path.exists() {
            menu_path
        } else {
            tracing::error!("Recipe file not found for deletion: {path}");
            return Err((
                StatusCode::NOT_FOUND,
                json_error(format!("Recipe file not found: {path}")),
            ));
        }
    };

    // Delete the file
    tokio::fs::remove_file(&file_path).await.map_err(|e| {
        tracing::error!("Failed to delete recipe file {}: {}", file_path, e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json_error(format!("Failed to delete recipe: {e}")),
        )
    })?;

    tracing::info!("Deleted recipe: {}", file_path);

    Ok(Json(serde_json::json!({
        "status": "success",
        "path": path
    })))
}

pub async fn recipe_image_upload(
    Path(path): Path<String>,
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    check_path(&path)?;

    let recipe_path = state.base_path.join(&path);

    let file_path = if recipe_path.exists() {
        recipe_path
    } else {
        let cook_path = Utf8PathBuf::from(format!("{}.cook", recipe_path));
        let menu_path = Utf8PathBuf::from(format!("{}.menu", recipe_path));
        if cook_path.exists() {
            cook_path
        } else if menu_path.exists() {
            menu_path
        } else {
            return Err((
                StatusCode::NOT_FOUND,
                json_error(format!("Recipe not found: {path}")),
            ));
        }
    };

    let stem = file_path.file_stem().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json_error("Invalid recipe path"),
        )
    })?;
    let dir = file_path.parent().unwrap_or(state.base_path.as_path());

    let field = multipart
        .next_field()
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                json_error(format!("Failed to read upload: {e}")),
            )
        })?
        .ok_or_else(|| (StatusCode::BAD_REQUEST, json_error("No file in upload")))?;

    let bytes = field.bytes().await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            json_error(format!("Failed to read file bytes: {e}")),
        )
    })?;

    let (image_bytes, ext) = process_image(&bytes)
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, json_error(e.to_string())))?;

    // Remove any existing same-stem images to avoid stale files taking priority
    for old_ext in ["jpg", "jpeg", "png", "webp"] {
        let old_path = dir.join(format!("{stem}.{old_ext}"));
        if old_path.exists() {
            tokio::fs::remove_file(&old_path).await.ok();
        }
    }

    let image_path = dir.join(format!("{stem}.{ext}"));
    tokio::fs::write(&image_path, &image_bytes)
        .await
        .map_err(|e| {
            tracing::error!("Failed to write image {}: {}", image_path, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                json_error(format!("Failed to save image: {e}")),
            )
        })?;

    tracing::info!("Saved image: {}", image_path);

    Ok(Json(serde_json::json!({
        "status": "success",
        "path": path
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_image_keeps_jpeg_unchanged() {
        // Minimal JPEG magic bytes (we only need format detection, not decoding)
        let jpeg_bytes = vec![
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00,
        ];
        let (out, ext) = process_image(&jpeg_bytes).unwrap();
        assert_eq!(ext, "jpg");
        assert_eq!(out, jpeg_bytes);
    }

    #[test]
    fn process_image_keeps_png_unchanged() {
        // Minimal PNG magic bytes
        let png_bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00];
        let (out, ext) = process_image(&png_bytes).unwrap();
        assert_eq!(ext, "png");
        assert_eq!(out, png_bytes);
    }

    #[test]
    fn process_image_converts_bmp_to_jpeg() {
        // Create a valid 1x1 BMP image using the image crate itself
        let img = image::DynamicImage::new_rgb8(1, 1);
        let mut bmp_bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut bmp_bytes),
            image::ImageFormat::Bmp,
        )
        .unwrap();

        let (out, ext) = process_image(&bmp_bytes).unwrap();
        assert_eq!(ext, "jpg");
        // JPEG output starts with FF D8
        assert_eq!(&out[0..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn process_image_rejects_invalid_bytes() {
        let garbage = vec![0x00, 0x01, 0x02, 0x03];
        assert!(process_image(&garbage).is_err());
    }
}
