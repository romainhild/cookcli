# Image Upload Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an "Upload Image" button to the recipe editor that saves an image next to the recipe file using Cooklang's same-stem naming convention.

**Architecture:** A new `POST /api/recipes/image/*path` endpoint (mirroring the existing `/api/recipes/raw/*path` pattern) accepts a multipart image upload, detects the format using the `image` crate, writes JPEG/PNG/WebP directly to disk, converts anything else to JPEG, and cleans up any previously uploaded same-stem image. The editor's header bar gets a button that triggers a hidden file input and POSTs via `fetch`.

**Tech Stack:** Rust `image` crate (format detection + conversion), `axum` multipart extractor, Tailwind CSS button, vanilla JS fetch with FormData.

---

## File Map

| File | Change |
|------|--------|
| `cookcli/Cargo.toml` | Add `image = "0.25"`, add `multipart` feature to `axum` |
| `src/server/handlers/recipes.rs` | Add `process_image()` helper + `recipe_image_upload` handler |
| `src/server/handlers/mod.rs` | Export `recipe_image_upload` |
| `src/server/mod.rs` | Register `POST /api/recipes/image/*path` with 10 MB body limit |
| `templates/edit.html` | Add upload button, hidden file input, and JS handler |

---

## Task 1: Add Dependencies

**Files:**
- Modify: `cookcli/Cargo.toml`

- [ ] **Step 1: Add `image` crate and enable `multipart` on `axum`**

In `cookcli/Cargo.toml`, change the `axum` line and add `image`:

```toml
axum = { version = "0.7", features = ["ws", "multipart"] }
image = "0.25"
```

- [ ] **Step 2: Verify the build compiles**

```bash
cd cookcli && cargo build 2>&1 | tail -5
```

Expected: compiles successfully (may take a minute to download and compile `image`).

- [ ] **Step 3: Commit**

```bash
git add cookcli/Cargo.toml cookcli/Cargo.lock
git commit -m "chore: add image crate and axum multipart feature"
```

---

## Task 2: Implement `process_image` with Tests

**Files:**
- Modify: `src/server/handlers/recipes.rs`

- [ ] **Step 1: Write the failing tests**

Add this test module at the bottom of `src/server/handlers/recipes.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_image_keeps_jpeg_unchanged() {
        // Minimal JPEG magic bytes (we only need format detection, not decoding)
        let jpeg_bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00];
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
```

- [ ] **Step 2: Run the tests to confirm they fail**

```bash
cd cookcli && cargo test process_image 2>&1
```

Expected: compilation error — `process_image` not defined yet.

- [ ] **Step 3: Add the `process_image` function**

Add these imports at the top of `src/server/handlers/recipes.rs` (alongside existing imports):

```rust
use image::ImageFormat;
```

Add the function body anywhere before the `#[cfg(test)]` block:

```rust
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
```

- [ ] **Step 4: Run the tests to confirm they pass**

```bash
cd cookcli && cargo test process_image 2>&1
```

Expected:
```
test handlers::recipes::tests::process_image_keeps_jpeg_unchanged ... ok
test handlers::recipes::tests::process_image_keeps_png_unchanged ... ok
test handlers::recipes::tests::process_image_converts_bmp_to_jpeg ... ok
test handlers::recipes::tests::process_image_rejects_invalid_bytes ... ok
```

- [ ] **Step 5: Commit**

```bash
git add src/server/handlers/recipes.rs
git commit -m "feat: add process_image helper with format detection and JPEG fallback"
```

---

## Task 3: Implement the Upload Handler

**Files:**
- Modify: `src/server/handlers/recipes.rs`
- Modify: `src/server/handlers/mod.rs`

- [ ] **Step 1: Add the multipart import**

Add to the imports at the top of `src/server/handlers/recipes.rs`:

```rust
use axum::extract::Multipart;
```

- [ ] **Step 2: Add the `recipe_image_upload` handler**

Add this function to `src/server/handlers/recipes.rs`, after `recipe_delete`:

```rust
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
    let dir = file_path
        .parent()
        .unwrap_or(state.base_path.as_path());

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

    let (image_bytes, ext) = process_image(&bytes).map_err(|e| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            json_error(e.to_string()),
        )
    })?;

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
        "path": image_path.to_string()
    })))
}
```

- [ ] **Step 3: Export the handler from `mod.rs`**

In `src/server/handlers/mod.rs`, change the `recipes` re-export line from:

```rust
pub use recipes::{all_recipes, recipe, recipe_delete, recipe_raw, recipe_save, reload, search};
```

to:

```rust
pub use recipes::{all_recipes, recipe, recipe_delete, recipe_image_upload, recipe_raw, recipe_save, reload, search};
```

- [ ] **Step 4: Verify it compiles**

```bash
cd cookcli && cargo build 2>&1 | tail -10
```

Expected: compiles with no errors.

- [ ] **Step 5: Commit**

```bash
git add src/server/handlers/recipes.rs src/server/handlers/mod.rs
git commit -m "feat: add recipe_image_upload handler"
```

---

## Task 4: Register the Route with 10 MB Body Limit

**Files:**
- Modify: `src/server/mod.rs`

- [ ] **Step 1: Add the route**

In `src/server/mod.rs`, in the `api()` function, add the new route **before** the existing `/recipes/*path` route (the comment already shows this pattern exists for `raw`):

```rust
.route("/recipes/raw/*path", get(handlers::recipe_raw)) // More specific route must come first
.route(
    "/recipes/image/*path",
    post(handlers::recipe_image_upload)
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024)),
) // More specific route must come first
.route(
    "/recipes/*path",
    get(handlers::recipe)
        .put(handlers::recipe_save)
        .delete(handlers::recipe_delete),
)
```

- [ ] **Step 2: Verify it compiles**

```bash
cd cookcli && cargo build 2>&1 | tail -10
```

Expected: compiles with no errors.

- [ ] **Step 3: Commit**

```bash
git add src/server/mod.rs
git commit -m "feat: register image upload route with 10 MB body limit"
```

---

## Task 5: Add the Upload Button to the Editor

**Files:**
- Modify: `templates/edit.html`

- [ ] **Step 1: Add the button and hidden file input to the header**

In `templates/edit.html`, find the `<div class="flex items-center gap-3">` that contains the save-status span and Delete button. Add the upload button and hidden input between the save-status span and the Delete button:

```html
        <div class="flex items-center gap-3">
            <span id="save-status" class="text-sm text-gray-500"></span>
            <button onclick="triggerImageUpload()" id="upload-image-btn" class="px-4 py-2 bg-orange-500 text-white rounded-lg hover:bg-orange-600 transition-colors flex items-center gap-2">
                <svg class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 16l4.586-4.586a2 2 0 012.828 0L16 16m-2-2l1.586-1.586a2 2 0 012.828 0L20 14m-6-6h.01M6 20h12a2 2 0 002-2V6a2 2 0 00-2-2H6a2 2 0 00-2 2v12a2 2 0 002 2z"></path>
                </svg>
                Upload Image
            </button>
            <input type="file" id="image-file-input" accept="image/*" class="hidden">
            <button onclick="showDeleteModal()" class="px-4 py-2 bg-red-500 text-white rounded-lg hover:bg-red-600 transition-colors flex items-center gap-2">
```

- [ ] **Step 2: Add the JavaScript**

At the bottom of the `<script>` block in `templates/edit.html`, just before the closing `</script>` tag, add:

```js
function triggerImageUpload() {
    document.getElementById('image-file-input').click();
}

document.getElementById('image-file-input').addEventListener('change', async function() {
    const file = this.files[0];
    if (!file) return;

    const btn = document.getElementById('upload-image-btn');
    const originalHTML = btn.innerHTML;
    btn.textContent = 'Uploading...';
    btn.disabled = true;

    const formData = new FormData();
    formData.append('image', file);

    try {
        const response = await fetch(
            `{{ prefix }}/api/recipes/image/${encodeURIComponent(recipePath)}`,
            { method: 'POST', body: formData }
        );
        if (response.ok) {
            showToast('Image saved', 'success');
        } else {
            const text = await response.text();
            showToast(`Upload failed: ${text || 'Unknown error'}`);
        }
    } catch (e) {
        showToast(`Upload failed: ${e.message}`);
    } finally {
        btn.innerHTML = originalHTML;
        btn.disabled = false;
        this.value = '';
    }
});
```

- [ ] **Step 3: Build CSS (no change needed — all Tailwind classes already exist in the project)**

```bash
cd cookcli && npm run build-css 2>&1 | tail -5
```

Expected: CSS compiles successfully.

- [ ] **Step 4: Commit**

```bash
git add templates/edit.html
git commit -m "feat: add image upload button to recipe editor"
```

---

## Task 6: Manual End-to-End Test

- [ ] **Step 1: Start the server**

```bash
cd cookcli && cargo run -- server ./seed
```

Expected: `Listening on http://127.0.0.1:9080`

- [ ] **Step 2: Open the editor for any recipe**

Navigate to `http://localhost:9080`, open any recipe, click "Edit".

- [ ] **Step 3: Upload a JPEG image**

Click "Upload Image", select any `.jpg` file. Verify:
- Button shows "Uploading..." briefly
- Green toast "Image saved" appears
- A `.jpg` file now exists next to the recipe `.cook` file in `./seed/`

- [ ] **Step 4: Verify the image appears on the recipe view**

Navigate back to the recipe view (Back button). Verify the recipe image is shown at the top.

- [ ] **Step 5: Upload a PNG — verify it replaces the JPEG**

Return to the editor, upload a `.png` file. Verify:
- The old `.jpg` file is gone from disk
- A new `.png` file exists next to the recipe

- [ ] **Step 6: Upload a large image (>1 MB, <10 MB)**

Upload an image over 1 MB. Verify it succeeds (confirms the 10 MB limit override works correctly).

- [ ] **Step 7: Try an unsupported format (e.g., a `.txt` file renamed to `.bmp` with garbage content)**

Verify the response shows a red toast with an error message ("Cannot detect image format").
