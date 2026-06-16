# Image Upload for Recipe Editor

**Date:** 2026-06-16

## Overview

Add an "Upload Image" button to the recipe editor that lets users attach a title image to a recipe. The image is saved next to the recipe file using Cooklang's native same-stem convention, so no metadata modification is required.

## Backend

### New route

```
POST /api/recipes/{path}/image
```

Registered alongside the existing recipe routes in `src/server/mod.rs`. This route has a **10 MB body limit** applied with `.layer(DefaultBodyLimit::max(10 * 1024 * 1024))` scoped to this route only. All other routes keep the existing 1 MB limit.

### Handler: `recipe_image_upload`

Located in `src/server/handlers/recipes.rs`.

Steps:
1. Call `check_path(&path)` to reject path traversal attempts (same guard used by all other recipe handlers).
2. Resolve the recipe file path from `state.base_path` and `path`, using the same extension-detection logic as `recipe_raw` (try exact path, then `.cook`, then `.menu`).
3. Read the image bytes from the multipart field using `axum::extract::Multipart` (requires enabling the `multipart` feature on the `axum` dependency).
4. Use the `image` crate to detect the format:
   - If JPEG, PNG, or WebP → write bytes directly to disk, preserving the original extension.
   - Any other format → decode with the `image` crate and re-encode as JPEG.
5. Delete any existing same-stem images (`{stem}.jpg`, `{stem}.jpeg`, `{stem}.png`, `{stem}.webp`) in the recipe directory before saving. This prevents stale images with a different extension from taking priority (Cooklang resolves title images in jpg → jpeg → png → webp order).
6. Save the image as `{recipe_stem}.{ext}` in the same directory as the recipe file.
7. Return `{"status": "success"}` on success, or an appropriate HTTP error status with a JSON error body.

### Dependencies

- Add `image` to `cookcli/Cargo.toml` with features: `jpeg`, `png`, `webp`.
- Enable the `multipart` feature on the existing `axum` dependency.

### Security

`check_path()` ensures the path URL parameter contains only normal path components (no `..` or absolute roots), preventing writes outside `base_path`.

## Frontend

### Changes to `templates/edit.html`

Add to the header bar, between the save-status span and the Delete button:

```html
<button onclick="triggerImageUpload()" id="upload-image-btn" class="...">
  <!-- camera icon SVG -->
  Upload Image
</button>
<input type="file" id="image-file-input" accept="image/*" class="hidden">
```

### JavaScript

```js
function triggerImageUpload() {
  document.getElementById('image-file-input').click();
}

document.getElementById('image-file-input').addEventListener('change', async function() {
  const file = this.files[0];
  if (!file) return;

  const btn = document.getElementById('upload-image-btn');
  btn.textContent = 'Uploading...';
  btn.disabled = true;

  const formData = new FormData();
  formData.append('image', file);

  try {
    const response = await fetch(
      `{{ prefix }}/api/recipes/${encodeURIComponent(recipePath)}/image`,
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
    btn.textContent = 'Upload Image';
    btn.disabled = false;
    this.value = '';
  }
});
```

## Behaviour Notes

- **Format conversion**: Formats supported natively by the `image` crate (JPEG, PNG, WebP, GIF, BMP, TIFF, etc.) can be decoded. Only JPEG, PNG, and WebP are written as-is; everything else is re-encoded as JPEG.
- **HEIC**: iOS Safari silently converts HEIC to JPEG before sending it through a file input, so the server never receives HEIC files.
- **Silent overwrite**: If a same-stem image already exists, it is replaced without confirmation, consistent with the editor's autosave behaviour.
- **No metadata change needed**: Cooklang's `RecipeEntry::title_image()` checks for a same-stem file before consulting YAML frontmatter, so the uploaded image is picked up automatically.
- **Body limit**: The 10 MB limit applies only to the image upload route. All other routes retain the existing 1 MB limit.
