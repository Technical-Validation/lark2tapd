//! Markdown 永远只解引用当前任务已批准的缓存图片，阻断任意文件、网络及 data URL。
use super::*;
use anyhow::{Result, ensure};
use std::io::Cursor;

fn bounded_image(bytes: &[u8]) -> Result<image::DynamicImage> {
    ensure!(bytes.len() <= 20 * 1024 * 1024, "Image exceeds 20 MiB");
    let reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let (w, h) = reader.into_dimensions()?;
    ensure!(
        w > 0 && h > 0 && w <= 8192 && h <= 8192 && u64::from(w) * u64::from(h) <= 16 * 1024 * 1024,
        "Image dimensions exceed the safe limit"
    );
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader.decode()?)
}

fn normalized_png(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut output = Cursor::new(Vec::new());
    bounded_image(bytes)?.write_to(&mut output, image::ImageFormat::Png)?;
    Ok(output.into_inner())
}

// GPUI 的渲染缓冲使用 BGRA；所有解码仍先经过相同的大小与内存限制。
fn rendered_image(bytes: &[u8]) -> Result<Arc<RenderImage>> {
    let mut pixels = bounded_image(bytes)?.into_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])))
}

fn image_reference(attachment: &DraftAttachment) -> String {
    let name = attachment
        .name
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace(['\n', '\r'], " ");
    format!("![{name}](attachment:{})", attachment.id)
}
fn blocked_image() -> Arc<Image> {
    Arc::new(Image::from_bytes(ImageFormat::Svg,br##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="24"><rect x="1" y="1" width="30" height="22" rx="3" fill="none" stroke="#888"/><path d="M4 20 L12 11 L19 17 L24 8 L29 20" fill="none" stroke="#888"/></svg>"##.to_vec()))
}

impl Workspace {
    pub(super) fn markdown(&self, id: &'static str, text: String) -> gpui_kit::base::TextView {
        let images = self.images.clone();
        let blocked = blocked_image();
        gpui_kit::base::TextView::markdown(id, text)
            .image_source(move |uri| {
                images
                    .get(uri.as_ref())
                    .cloned()
                    .map(ImageSource::Render)
                    .unwrap_or_else(|| ImageSource::Image(blocked.clone()))
            })
            .on_link_click(|_, _, _, _| {})
    }
    pub(super) fn add_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(guid) = self.selected.clone() else {
            return;
        };
        let profile = self.edit_profile.clone();
        let session = self.editor_session;
        let result = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(self.tr(cx, "添加附件", "Add attachments").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = result.await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.selected.as_ref() != Some(&guid)
                    || this.edit_profile != profile
                    || this.editor_session != session
                {
                    return;
                }
                match picked {
                    Ok(Ok(Some(paths))) => {
                        this.queue_files(&guid, paths, cx);
                    }
                    Ok(Ok(None)) => {}
                    _ => this.form_error(
                        "attachments",
                        this.tr(cx, "无法读取文件选择结果", "Could not read file selection")
                            .into(),
                        cx,
                    ),
                }
            });
        })
        .detach();
    }
    pub(super) fn paste_image(
        &mut self,
        item: &ClipboardItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.can_edit_description(cx) {
            return true;
        }
        // Finder 复制图片提供文件条目而不是图像条目；不能回退成泄露本机路径的文本。
        if let Some(paths) = item.entries().iter().find_map(|e| {
            if let ClipboardEntry::ExternalPaths(p) = e {
                Some(p.paths())
            } else {
                None
            }
        }) {
            let Some(guid) = self.selected.clone() else {
                return true;
            };
            self.queue_files(&guid, paths.to_vec(), cx);
            return true;
        }
        let Some(image) = item.entries().iter().find_map(|e| {
            if let ClipboardEntry::Image(i) = e {
                Some(i)
            } else {
                None
            }
        }) else {
            return false;
        };
        let Some(guid) = self.selected.clone() else {
            return false;
        };
        if self.attachments.len() + self.pending_pastes.len() + self.pending_files >= 10 {
            self.form_error(
                "attachments",
                self.tr(
                    cx,
                    "每个任务最多 10 个附件",
                    "At most 10 attachments per task",
                )
                .into(),
                cx,
            );
            return true;
        }
        let bytes = image.bytes().to_vec();
        let profile = self.edit_profile.clone();
        let session = self.editor_session;
        self.next_paste += 1;
        let marker = format!(
            "![{}](attachment:pending-{}-{})",
            self.tr(cx, "正在处理图片", "Processing image"),
            session,
            self.next_paste
        );
        self.pending_pastes.insert(marker.clone());
        self.description.update(cx, |s, cx| {
            s.replace_text_in_range(None, &marker, window, cx)
        });
        let conversion = cx.background_executor().spawn(async move {
            let png = normalized_png(&bytes)?;
            ensure!(
                png.len() < lark2tapd_bridge::MAX_INLINE_IMAGE_BYTES,
                "Pasted image must be below 5 MB after PNG conversion"
            );
            Ok::<_, anyhow::Error>(png)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = conversion.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.selected.as_ref() != Some(&guid)
                    || this.edit_profile != profile
                    || this.editor_session != session
                {
                    this.pending_pastes.remove(&marker);
                    return;
                }
                match result {
                    Ok(bytes) => this.send_editor(
                        Job::PastedImage {
                            guid,
                            bytes,
                            extension: "png".into(),
                            marker,
                            session,
                        },
                        cx,
                    ),
                    Err(error) => {
                        this.reject_attachment(
                            &MediaFailure {
                                guid,
                                profile,
                                session,
                                marker: Some(marker),
                                message: error.to_string(),
                            },
                            window,
                            cx,
                        );
                    }
                }
            });
        })
        .detach();
        true
    }
    pub(super) fn accept_attachment(
        &mut self,
        ready: &MediaReady,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected.as_ref() != Some(&ready.guid)
            || self.edit_profile != ready.profile
            || self.editor_session != ready.session
        {
            return;
        }
        if self.description.read(cx).is_composing() {
            self.deferred_media
                .push(DeferredMedia::Ready(ready.clone()));
            return;
        }
        if ready.marker.is_none() {
            self.finish_file();
        }
        if let Some(marker) = &ready.marker {
            self.pending_pastes.remove(marker);
            if !self.description.read(cx).text().contains(marker) {
                // 用户已删除图片：不重新插入，也清理历史里的失效占位符。
                self.description.update(cx, |s, cx| {
                    s.resolve_placeholder(marker, "", cx);
                });
                return;
            }
        }
        if !self.attachments.iter().any(|a| a.id == ready.attachment.id) {
            if self.attachments.len() >= 10 {
                if let Some(marker) = &ready.marker {
                    self.description.update(cx, |s, cx| {
                        s.resolve_placeholder(marker, "", cx);
                    });
                }
                self.form_error(
                    "attachments",
                    self.tr(
                        cx,
                        "每个任务最多 10 个附件",
                        "At most 10 attachments per task",
                    )
                    .into(),
                    cx,
                );
                return;
            }
            self.attachments.push(ready.attachment.clone());
        }
        if let Some(marker) = &ready.marker {
            self.description.update(cx, |s, cx| {
                s.resolve_placeholder(marker, &image_reference(&ready.attachment), cx);
            });
        } else if ready.attachment.is_image {
            let text = format!("\n\n{}", image_reference(&ready.attachment));
            self.description
                .update(cx, |s, cx| s.append_text(&text, cx));
        }
        self.load_images(window, cx);
        cx.notify();
    }
    pub(super) fn reject_attachment(
        &mut self,
        failure: &MediaFailure,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected.as_ref() != Some(&failure.guid)
            || self.edit_profile != failure.profile
            || self.editor_session != failure.session
        {
            return;
        }
        if self.description.read(cx).is_composing() {
            self.deferred_media
                .push(DeferredMedia::Failed(failure.clone()));
            return;
        }
        if failure.marker.is_none() {
            self.finish_file();
        }
        if let Some(marker) = &failure.marker {
            self.pending_pastes.remove(marker);
            self.description.update(cx, |s, cx| {
                s.resolve_placeholder(marker, "", cx);
            });
        }
        self.form_error("attachments", failure.message.clone(), cx);
        cx.notify();
    }
    fn finish_file(&mut self) {
        self.pending_files = self.pending_files.saturating_sub(1);
        if self.pending_files == 0 {
            self.pending_file_paths.clear();
        }
    }
    fn queue_files(&mut self, guid: &str, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let paths: BTreeSet<_> = paths
            .into_iter()
            .filter(|p| !self.pending_file_paths.contains(p))
            .collect();
        if self.attachments.len() + self.pending_files + self.pending_pastes.len() + paths.len()
            > 10
        {
            self.form_error(
                "attachments",
                self.tr(
                    cx,
                    "每个任务最多 10 个附件",
                    "At most 10 attachments per task",
                )
                .into(),
                cx,
            );
            return;
        }
        for path in paths {
            self.pending_file_paths.insert(path.clone());
            self.pending_files += 1;
            self.send_editor(
                Job::LocalFile {
                    guid: guid.into(),
                    path,
                    session: self.editor_session,
                },
                cx,
            );
        }
        cx.notify();
    }
    pub(super) fn load_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root =
            match crate::worker::media_root(&self.model.read(cx).data_root, &self.edit_profile) {
                Ok(root) => root,
                Err(_) => return,
            };
        for attachment in self.attachments.clone().into_iter().filter(|a| a.is_image) {
            let key = format!("attachment:{}", attachment.id);
            if self.images.contains_key(&key) || !self.loading_images.insert(key.clone()) {
                continue;
            }
            let root = root.clone();
            let guid = self.selected.clone();
            let profile = self.edit_profile.clone();
            let session = self.editor_session;
            let attachment_id = attachment.id.clone();
            let read = cx.background_executor().spawn(async move {
                let bytes = lark2tapd_core::read_attachment(&root, &attachment)?;
                rendered_image(&bytes)
            });
            cx.spawn_in(window, async move |this, cx| {
                let result = read.await;
                let _ = this.update_in(cx, |this, _, cx| {
                    if this.selected != guid
                        || this.edit_profile != profile
                        || this.editor_session != session
                    {
                        return;
                    }
                    this.loading_images.remove(&key);
                    if !this.attachments.iter().any(|a| a.id == attachment_id) {
                        return;
                    }
                    match result {
                        Ok(image) => {
                            this.images.insert(key, image);
                            this.refresh_editor_images(cx);
                        }
                        Err(e) => this.form_error("attachments", e.to_string(), cx),
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::prelude::v1::test;
    #[test]
    fn image_decoder_rejects_invalid_and_oversized_payloads() {
        assert!(normalized_png(b"not an image").is_err());
        assert!(normalized_png(&vec![0; 20 * 1024 * 1024 + 1]).is_err());
    }
    #[test]
    fn native_render_image_uses_bgra_without_losing_alpha() {
        let mut bytes = Cursor::new(Vec::new());
        let rgba = image::RgbaImage::from_pixel(1, 1, image::Rgba([240, 30, 10, 128]));
        image::DynamicImage::ImageRgba8(rgba)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let image = rendered_image(bytes.get_ref()).unwrap();
        assert_eq!(image.as_bytes(0).unwrap(), &[10, 30, 240, 128]);
        assert_eq!(image.size(0), size(DevicePixels(1), DevicePixels(1)));
        assert!(rendered_image(b"not an image").is_err());
    }

    #[test]
    fn valid_image_is_converted_to_bounded_png() {
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let encoded = normalized_png(bytes.get_ref()).unwrap();
        assert_eq!(&encoded[..8], b"\x89PNG\r\n\x1a\n");
    }
}
