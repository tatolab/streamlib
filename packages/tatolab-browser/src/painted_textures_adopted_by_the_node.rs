// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The textures the browser host's page paints into, adopted once each and
//! found again as CEF paints into them in turn.

use std::collections::VecDeque;
use std::fs::File;
use std::os::unix::fs::MetadataExt;

/// How many painted textures one tab keeps adopted; CEF paints a page into a
/// handful in turn, and one resized away is let go once past this.
pub const MOST_PAINTED_TEXTURES_ADOPTED_BY_ONE_TAB: usize = 8;

/// The DMA-BUF a painted texture lives in, as `fstat` names it: each handover
/// hands a new descriptor, and the buffer behind it stays the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaintedTextureDmaBufIdentity {
    pub device: u64,
    pub inode: u64,
}

impl PaintedTextureDmaBufIdentity {
    pub fn of(dma_buf: &File) -> std::io::Result<Self> {
        let metadata = dma_buf.metadata()?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

/// Where a painted texture's pixels sit in its DMA-BUF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaintedTextureLayout {
    pub dma_buf_identity: PaintedTextureDmaBufIdentity,
    pub width: u32,
    pub height: u32,
    pub pixel_format: String,
    pub allocation_byte_size: u64,
    pub plane_offsets_bytes: Vec<u64>,
    pub plane_strides_bytes: Vec<u64>,
    pub drm_format_modifier: u64,
}

/// One painted texture as the node adopted it, held with the DMA-BUF it was
/// adopted from.
pub struct PaintedTextureAdoptedByTheNode<AdoptedSurface> {
    pub adopted_surface: AdoptedSurface,
    _adopted_dma_buf: File,
}

/// The painted textures one tab connection adopted, oldest first.
pub struct PaintedTexturesAdoptedByTheNode<AdoptedSurface> {
    adopted: VecDeque<(
        PaintedTextureLayout,
        PaintedTextureAdoptedByTheNode<AdoptedSurface>,
    )>,
}

impl<AdoptedSurface> Default for PaintedTexturesAdoptedByTheNode<AdoptedSurface> {
    fn default() -> Self {
        Self {
            adopted: VecDeque::new(),
        }
    }
}

impl<AdoptedSurface> PaintedTexturesAdoptedByTheNode<AdoptedSurface> {
    /// The surface adopted for `layout`, adopting `dma_buf` through `adopt`
    /// the first time the layout is seen; past the bound the oldest is let go.
    pub fn adopted_or_adopt<Refusal>(
        &mut self,
        layout: PaintedTextureLayout,
        dma_buf: File,
        adopt: impl FnOnce(&File) -> Result<AdoptedSurface, Refusal>,
    ) -> Result<&AdoptedSurface, Refusal> {
        let index = match self
            .adopted
            .iter()
            .position(|(adopted_layout, _)| *adopted_layout == layout)
        {
            Some(index) => index,
            None => {
                let adopted_surface = adopt(&dma_buf)?;
                if self.adopted.len() == MOST_PAINTED_TEXTURES_ADOPTED_BY_ONE_TAB {
                    self.adopted.pop_front();
                }
                self.adopted.push_back((
                    layout,
                    PaintedTextureAdoptedByTheNode {
                        adopted_surface,
                        _adopted_dma_buf: dma_buf,
                    },
                ));
                self.adopted.len() - 1
            }
        };
        Ok(&self.adopted[index].1.adopted_surface)
    }

    pub fn adopted_count(&self) -> usize {
        self.adopted.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout_of(inode: u64) -> PaintedTextureLayout {
        PaintedTextureLayout {
            dma_buf_identity: PaintedTextureDmaBufIdentity { device: 1, inode },
            width: 2,
            height: 1,
            pixel_format: "bgra32".to_string(),
            allocation_byte_size: 8,
            plane_offsets_bytes: vec![0],
            plane_strides_bytes: vec![8],
            drm_format_modifier: 0,
        }
    }

    fn a_descriptor() -> File {
        File::open("/dev/null").unwrap()
    }

    #[test]
    fn a_texture_painted_into_again_is_adopted_once() {
        let mut adopted = PaintedTexturesAdoptedByTheNode::default();
        let mut adoptions = 0;
        for _ in 0..3 {
            let surface = adopted
                .adopted_or_adopt(layout_of(7), a_descriptor(), |_| {
                    adoptions += 1;
                    Ok::<_, String>(70)
                })
                .unwrap();
            assert_eq!(*surface, 70);
        }
        assert_eq!(adoptions, 1);
        assert_eq!(adopted.adopted_count(), 1);
    }

    #[test]
    fn the_same_buffer_in_another_layout_is_adopted_again() {
        let mut adopted = PaintedTexturesAdoptedByTheNode::default();
        adopted
            .adopted_or_adopt(layout_of(7), a_descriptor(), |_| Ok::<_, String>(1))
            .unwrap();
        let mut resized = layout_of(7);
        resized.width = 4;
        resized.plane_strides_bytes = vec![16];
        let surface = adopted
            .adopted_or_adopt(resized, a_descriptor(), |_| Ok::<_, String>(2))
            .unwrap();
        assert_eq!(*surface, 2);
    }

    #[test]
    fn past_the_bound_the_oldest_adoption_is_let_go() {
        let mut adopted = PaintedTexturesAdoptedByTheNode::default();
        for inode in 0..=MOST_PAINTED_TEXTURES_ADOPTED_BY_ONE_TAB as u64 {
            adopted
                .adopted_or_adopt(layout_of(inode), a_descriptor(), |_| Ok::<_, String>(inode))
                .unwrap();
        }
        assert_eq!(
            adopted.adopted_count(),
            MOST_PAINTED_TEXTURES_ADOPTED_BY_ONE_TAB
        );
        let mut adopted_again = false;
        adopted
            .adopted_or_adopt(layout_of(0), a_descriptor(), |_| {
                adopted_again = true;
                Ok::<_, String>(0)
            })
            .unwrap();
        assert!(adopted_again);
    }

    #[test]
    fn a_refused_adoption_is_not_kept() {
        let mut adopted = PaintedTexturesAdoptedByTheNode::<u32>::default();
        let refused = adopted.adopted_or_adopt(layout_of(7), a_descriptor(), |_| {
            Err("only the linear modifier".to_string())
        });
        assert_eq!(refused.err().as_deref(), Some("only the linear modifier"));
        assert_eq!(adopted.adopted_count(), 0);
    }

    #[test]
    fn a_dma_buf_is_named_by_the_file_behind_its_descriptor() {
        let file = File::open("/dev/null").unwrap();
        let duplicate = file.try_clone().unwrap();
        assert_eq!(
            PaintedTextureDmaBufIdentity::of(&file).unwrap(),
            PaintedTextureDmaBufIdentity::of(&duplicate).unwrap()
        );
    }
}
