#[cfg(not(target_os = "windows"))]
use std::collections::HashMap;
#[cfg(not(target_os = "windows"))]
use std::sync::Arc;

#[cfg(not(target_os = "windows"))]
use loadngo_host_core::DecodedImage;
#[cfg(not(target_os = "windows"))]
use loadngo_renderer::{FrameCommand, GraphicsBackend, RendererError};

#[cfg(target_os = "windows")]
mod windows_backend {
    use std::collections::HashMap;
    use std::ffi::CString;
    use std::mem::{size_of, ManuallyDrop};
    use std::ptr::copy_nonoverlapping;
    use std::sync::Arc;

    use blake3::Hasher;
    use loadngo_host_core::DecodedImage;
    use loadngo_renderer::{FrameCommand, GraphicsBackend, ImageRequest, RendererError};
    use ui_core::geometry::{Color, Rect};
    use windows::core::{s, Interface, PCSTR};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, RECT};
    use windows::Win32::Graphics::Direct3D::{
        D3D_FEATURE_LEVEL_11_0, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob,
    };
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};

    const FRAME_COUNT: usize = 2;
    const MAX_TEXTURES: u32 = 4096;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Dx12BackendState {
        UnboundSurface,
        Ready,
        SurfaceBound,
    }

    #[allow(dead_code)]
    #[derive(Clone, Copy, Debug)]
    #[repr(C)]
    struct Vertex {
        position: [f32; 2],
        uv: [f32; 2],
        color: [f32; 4],
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum PipelineKind {
        Solid,
        Textured,
    }

    #[derive(Clone, Copy, Debug)]
    struct DrawItem {
        first_vertex: u32,
        vertex_count: u32,
        pipeline: PipelineKind,
        descriptor_index: u32,
        scissor: RECT,
    }

    #[allow(dead_code)]
    #[derive(Clone)]
    struct Dx12Texture {
        // Keep the resource alive for the descriptor heap entry that points at it.
        resource: ID3D12Resource,
        descriptor_index: u32,
        width: u32,
        height: u32,
        content_hash: [u8; 32],
    }

    pub struct Dx12Backend {
        state: Dx12BackendState,
        recorded_commands: Vec<FrameCommand>,
        frame_open: bool,
        surface_width: i32,
        surface_height: i32,
        image_resources: HashMap<String, Arc<DecodedImage>>,
        device: ID3D12Device,
        command_queue: ID3D12CommandQueue,
        command_allocator: ID3D12CommandAllocator,
        command_list: ID3D12GraphicsCommandList,
        swap_chain: IDXGISwapChain3,
        rtv_heap: ID3D12DescriptorHeap,
        rtv_descriptor_size: u32,
        srv_heap: ID3D12DescriptorHeap,
        srv_descriptor_size: u32,
        back_buffers: Vec<ID3D12Resource>,
        frame_index: u32,
        fence: ID3D12Fence,
        fence_value: u64,
        fence_event: HANDLE,
        root_signature: ID3D12RootSignature,
        solid_pipeline: ID3D12PipelineState,
        textured_pipeline: ID3D12PipelineState,
        vertex_buffer: Option<ID3D12Resource>,
        vertex_buffer_capacity: usize,
        textures: HashMap<String, Dx12Texture>,
        next_descriptor_index: u32,
    }

    impl Dx12Backend {
        pub fn try_bind_hwnd(hwnd: isize, width: i32, height: i32) -> Result<Self, RendererError> {
            unsafe {
                let factory: IDXGIFactory4 =
                    CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).map_err(|err| {
                    RendererError::Backend(format!("CreateDXGIFactory2 failed: {err}"))
                })?;
                let mut device: Option<ID3D12Device> = None;
                D3D12CreateDevice(None, D3D_FEATURE_LEVEL_11_0, &mut device).map_err(|err| {
                    RendererError::Backend(format!("D3D12CreateDevice failed: {err}"))
                })?;
                let device = device.ok_or_else(|| {
                    RendererError::Backend("D3D12CreateDevice returned no device".to_string())
                })?;

                let queue_desc = D3D12_COMMAND_QUEUE_DESC {
                    Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                    Priority: D3D12_COMMAND_QUEUE_PRIORITY_NORMAL.0,
                    Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
                    NodeMask: 0,
                };
                let command_queue = device.CreateCommandQueue(&queue_desc).map_err(|err| {
                    RendererError::Backend(format!("CreateCommandQueue failed: {err}"))
                })?;
                let command_allocator = device
                    .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
                    .map_err(|err| {
                        RendererError::Backend(format!("CreateCommandAllocator failed: {err}"))
                    })?;
                let command_list: ID3D12GraphicsCommandList = device
                    .CreateCommandList(
                        0,
                        D3D12_COMMAND_LIST_TYPE_DIRECT,
                        &command_allocator,
                        None,
                    )
                    .map_err(|err| {
                        RendererError::Backend(format!("CreateCommandList failed: {err}"))
                    })?;
                command_list.Close().map_err(|err| {
                    RendererError::Backend(format!(
                        "initial command list close failed: {err}"
                    ))
                })?;

                let swap_desc = DXGI_SWAP_CHAIN_DESC1 {
                    Width: width.max(1) as u32,
                    Height: height.max(1) as u32,
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    Stereo: false.into(),
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                    BufferCount: FRAME_COUNT as u32,
                    Scaling: DXGI_SCALING_STRETCH,
                    SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                    AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                    Flags: 0,
                };
                let swap_chain: IDXGISwapChain3 = factory
                    .CreateSwapChainForHwnd(
                        &command_queue,
                        HWND(hwnd as *mut _),
                        &swap_desc,
                        None,
                        None,
                    )
                    .and_then(|value| value.cast())
                    .map_err(|err| {
                        RendererError::Backend(format!(
                            "CreateSwapChainForHwnd failed: {err}"
                        ))
                    })?;
                let _ = factory.MakeWindowAssociation(HWND(hwnd as *mut _), DXGI_MWA_NO_ALT_ENTER);

                let rtv_heap = device
                    .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                        Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                        NumDescriptors: FRAME_COUNT as u32,
                        Flags: D3D12_DESCRIPTOR_HEAP_FLAG_NONE,
                        NodeMask: 0,
                    })
                    .map_err(|err| {
                        RendererError::Backend(format!(
                            "CreateDescriptorHeap RTV failed: {err}"
                        ))
                    })?;
                let srv_heap = device
                    .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                        Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                        NumDescriptors: MAX_TEXTURES,
                        Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                        NodeMask: 0,
                    })
                    .map_err(|err| {
                        RendererError::Backend(format!(
                            "CreateDescriptorHeap SRV failed: {err}"
                        ))
                    })?;
                let fence = device
                    .CreateFence(0, D3D12_FENCE_FLAG_NONE)
                    .map_err(|err| RendererError::Backend(format!("CreateFence failed: {err}")))?;
                let fence_event = CreateEventW(None, false, false, None).map_err(|err| {
                    RendererError::Backend(format!("CreateEventW failed: {err}"))
                })?;

                let root_signature = create_root_signature(&device)?;
                let solid_pipeline = create_pipeline_state(&device, &root_signature, false)?;
                let textured_pipeline = create_pipeline_state(&device, &root_signature, true)?;

                let mut backend = Self {
                    state: Dx12BackendState::SurfaceBound,
                    recorded_commands: Vec::new(),
                    frame_open: false,
                    surface_width: width.max(1),
                    surface_height: height.max(1),
                    image_resources: HashMap::new(),
                    device,
                    command_queue,
                    command_allocator,
                    command_list,
                    swap_chain,
                    rtv_heap,
                    rtv_descriptor_size: 0,
                    srv_heap,
                    srv_descriptor_size: 0,
                    back_buffers: Vec::new(),
                    frame_index: 0,
                    fence,
                    fence_value: 1,
                    fence_event,
                    root_signature,
                    solid_pipeline,
                    textured_pipeline,
                    vertex_buffer: None,
                    vertex_buffer_capacity: 0,
                    textures: HashMap::new(),
                    next_descriptor_index: 1,
                };
                backend.rtv_descriptor_size = backend
                    .device
                    .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV);
                backend.srv_descriptor_size = backend
                    .device
                    .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV);
                backend.recreate_back_buffers()?;
                Ok(backend)
            }
        }

        pub fn state(&self) -> Dx12BackendState {
            self.state
        }

        pub fn update_surface_size(&mut self, width: i32, height: i32) {
            self.surface_width = width.max(1);
            self.surface_height = height.max(1);
        }

        pub fn supports_commands(&self, commands: &[FrameCommand]) -> bool {
            commands.iter().all(|command| {
                matches!(
                    command,
                    FrameCommand::Clear { .. }
                        | FrameCommand::FillRect { .. }
                        | FrameCommand::StrokeRect { .. }
                        | FrameCommand::Image(_)
                )
            })
        }

        pub fn sync_image_resources(
            &mut self,
            resources: impl IntoIterator<Item = (String, Arc<DecodedImage>)>,
        ) {
            self.image_resources.clear();
            for (key, image) in resources {
                self.image_resources.insert(key, image);
            }
        }
    }

    impl GraphicsBackend for Dx12Backend {
        fn begin_frame(&mut self) -> Result<(), RendererError> {
            self.frame_open = true;
            self.recorded_commands.clear();
            Ok(())
        }

        fn submit(&mut self, commands: &[FrameCommand]) -> Result<(), RendererError> {
            if !self.frame_open {
                return Err(RendererError::Backend(
                    "cannot submit DX12 commands outside an open frame".to_string(),
                ));
            }
            self.recorded_commands.extend_from_slice(commands);
            Ok(())
        }

        fn end_frame(&mut self) -> Result<(), RendererError> {
            if !self.frame_open {
                return Err(RendererError::Backend(
                    "cannot end a DX12 frame that was never opened".to_string(),
                ));
            }
            self.frame_open = false;
            self.draw_frame()
        }
    }

    impl Drop for Dx12Backend {
        fn drop(&mut self) {
            let _ = self.wait_for_gpu();
            unsafe {
                let _ = CloseHandle(self.fence_event);
            }
        }
    }

    impl Dx12Backend {
        fn recreate_back_buffers(&mut self) -> Result<(), RendererError> {
            unsafe {
                self.back_buffers.clear();
                let base = self.rtv_heap.GetCPUDescriptorHandleForHeapStart();
                for index in 0..FRAME_COUNT {
                    let resource: ID3D12Resource =
                        self.swap_chain.GetBuffer(index as u32).map_err(|err| {
                            RendererError::Backend(format!(
                                "swapchain GetBuffer failed: {err}"
                            ))
                        })?;
                    let handle = D3D12_CPU_DESCRIPTOR_HANDLE {
                        ptr: base.ptr + index * self.rtv_descriptor_size as usize,
                    };
                    self.device.CreateRenderTargetView(&resource, None, handle);
                    self.back_buffers.push(resource);
                }
                self.frame_index = self.swap_chain.GetCurrentBackBufferIndex();
                Ok(())
            }
        }

        fn resize_if_needed(&mut self) -> Result<(), RendererError> {
            unsafe {
                let desc = self.swap_chain.GetDesc1().map_err(|err| {
                    RendererError::Backend(format!("GetDesc1 failed: {err}"))
                })?;
                if desc.Width == self.surface_width as u32
                    && desc.Height == self.surface_height as u32
                {
                    return Ok(());
                }
                self.wait_for_gpu()?;
                self.back_buffers.clear();
                self.swap_chain
                    .ResizeBuffers(
                        FRAME_COUNT as u32,
                        self.surface_width as u32,
                        self.surface_height as u32,
                        DXGI_FORMAT_R8G8B8A8_UNORM,
                        DXGI_SWAP_CHAIN_FLAG(0),
                    )
                    .map_err(|err| {
                        RendererError::Backend(format!("ResizeBuffers failed: {err}"))
                    })?;
                self.recreate_back_buffers()
            }
        }

        fn wait_for_gpu(&mut self) -> Result<(), RendererError> {
            unsafe {
                let value = self.fence_value;
                self.command_queue.Signal(&self.fence, value).map_err(|err| {
                    RendererError::Backend(format!("queue Signal failed: {err}"))
                })?;
                self.fence_value = self.fence_value.saturating_add(1);
                if self.fence.GetCompletedValue() < value {
                    self.fence
                        .SetEventOnCompletion(value, self.fence_event)
                        .map_err(|err| {
                            RendererError::Backend(format!(
                                "SetEventOnCompletion failed: {err}"
                            ))
                        })?;
                    WaitForSingleObject(self.fence_event, INFINITE);
                }
                self.frame_index = self.swap_chain.GetCurrentBackBufferIndex();
                Ok(())
            }
        }

        fn ensure_vertex_buffer(&mut self, required_size: usize) -> Result<(), RendererError> {
            if self.vertex_buffer_capacity >= required_size {
                return Ok(());
            }
            let capacity = required_size.next_power_of_two().max(4096);
            unsafe {
                let desc = D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                    Alignment: 0,
                    Width: capacity as u64,
                    Height: 1,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: DXGI_FORMAT_UNKNOWN,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                    Flags: D3D12_RESOURCE_FLAG_NONE,
                };
                let mut resource: Option<ID3D12Resource> = None;
                self.device
                    .CreateCommittedResource(
                        &heap_properties(D3D12_HEAP_TYPE_UPLOAD),
                        D3D12_HEAP_FLAG_NONE,
                        &desc,
                        D3D12_RESOURCE_STATE_GENERIC_READ,
                        None,
                        &mut resource,
                    )
                    .map_err(|err| {
                        RendererError::Backend(format!(
                            "vertex buffer allocation failed: {err}"
                        ))
                    })?;
                let resource = resource.ok_or_else(|| {
                    RendererError::Backend("vertex buffer allocation returned no resource".to_string())
                })?;
                self.vertex_buffer = Some(resource);
                self.vertex_buffer_capacity = capacity;
                Ok(())
            }
        }

        fn ensure_texture(
            &mut self,
            key: &str,
            image: &DecodedImage,
        ) -> Result<u32, RendererError> {
            let content_hash = image_hash(image);
            if let Some(existing) = self.textures.get(key) {
                if existing.content_hash == content_hash
                    && existing.width == image.width
                    && existing.height == image.height
                {
                    return Ok(existing.descriptor_index);
                }
            }
            let descriptor_index = self
                .textures
                .get(key)
                .map(|value| value.descriptor_index)
                .unwrap_or_else(|| {
                    let next = self.next_descriptor_index;
                    self.next_descriptor_index = self.next_descriptor_index.saturating_add(1);
                    next
                });
            let texture = unsafe { self.upload_texture(image, descriptor_index)? };
            self.textures.insert(
                key.to_string(),
                Dx12Texture {
                    resource: texture,
                    descriptor_index,
                    width: image.width,
                    height: image.height,
                    content_hash,
                },
            );
            Ok(descriptor_index)
        }

        unsafe fn upload_texture(
            &mut self,
            image: &DecodedImage,
            descriptor_index: u32,
        ) -> Result<ID3D12Resource, RendererError> {
            let texture_desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Alignment: 0,
                Width: image.width as u64,
                Height: image.height,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
                Flags: D3D12_RESOURCE_FLAG_NONE,
            };
            let mut texture: Option<ID3D12Resource> = None;
            self.device
                .CreateCommittedResource(
                    &heap_properties(D3D12_HEAP_TYPE_DEFAULT),
                    D3D12_HEAP_FLAG_NONE,
                    &texture_desc,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                    None,
                    &mut texture,
                )
                .map_err(|err| {
                    RendererError::Backend(format!("texture allocation failed: {err}"))
                })?;
            let texture = texture.ok_or_else(|| {
                RendererError::Backend("texture allocation returned no resource".to_string())
            })?;

            let row_pitch = ((image.width as usize * 4) + 255) & !255usize;
            let upload_size = row_pitch * image.height as usize;
            let upload_desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Alignment: 0,
                Width: upload_size as u64,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: DXGI_FORMAT_UNKNOWN,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                Flags: D3D12_RESOURCE_FLAG_NONE,
            };
            let mut upload: Option<ID3D12Resource> = None;
            self.device
                .CreateCommittedResource(
                    &heap_properties(D3D12_HEAP_TYPE_UPLOAD),
                    D3D12_HEAP_FLAG_NONE,
                    &upload_desc,
                    D3D12_RESOURCE_STATE_GENERIC_READ,
                    None,
                    &mut upload,
                )
                .map_err(|err| {
                    RendererError::Backend(format!("upload allocation failed: {err}"))
                })?;
            let upload = upload.ok_or_else(|| {
                RendererError::Backend("upload allocation returned no resource".to_string())
            })?;

            let mut mapped = std::ptr::null_mut::<std::ffi::c_void>();
            upload.Map(0, None, Some(&mut mapped)).map_err(|err| {
                RendererError::Backend(format!("upload map failed: {err}"))
            })?;
            for row in 0..image.height as usize {
                let dst = (mapped as *mut u8).add(row * row_pitch);
                let src = image.rgba8.as_ptr().add(row * image.width as usize * 4);
                copy_nonoverlapping(src, dst, image.width as usize * 4);
            }
            upload.Unmap(0, None);

            self.command_allocator.Reset().map_err(|err| {
                RendererError::Backend(format!("command allocator reset failed: {err}"))
            })?;
            self.command_list
                .Reset(&self.command_allocator, None)
                .map_err(|err| {
                    RendererError::Backend(format!("command list reset failed: {err}"))
                })?;

            let dst_location = D3D12_TEXTURE_COPY_LOCATION {
                pResource: ManuallyDrop::new(Some(texture.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
            };
            let src_location = D3D12_TEXTURE_COPY_LOCATION {
                pResource: ManuallyDrop::new(Some(upload.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                        Offset: 0,
                        Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                            Width: image.width,
                            Height: image.height,
                            Depth: 1,
                            RowPitch: row_pitch as u32,
                        },
                    },
                },
            };
            self.command_list
                .CopyTextureRegion(&dst_location, 0, 0, 0, &src_location, None);
            self.command_list.ResourceBarrier(&[transition_barrier(
                &texture,
                D3D12_RESOURCE_STATE_COPY_DEST,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
            )]);
            self.command_list.Close().map_err(|err| {
                RendererError::Backend(format!("texture upload close failed: {err}"))
            })?;
            let list: ID3D12CommandList = self.command_list.cast().map_err(|err| {
                RendererError::Backend(format!("command list cast failed: {err}"))
            })?;
            self.command_queue.ExecuteCommandLists(&[Some(list)]);
            self.wait_for_gpu()?;

            let handle = srv_cpu_handle(&self.srv_heap, self.srv_descriptor_size, descriptor_index);
            self.device.CreateShaderResourceView(
                Some(&texture),
                Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                    Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                    Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                        Texture2D: D3D12_TEX2D_SRV {
                            MostDetailedMip: 0,
                            MipLevels: 1,
                            PlaneSlice: 0,
                            ResourceMinLODClamp: 0.0,
                        },
                    },
                }),
                handle,
            );

            Ok(texture)
        }

        fn build_draws(
            &mut self,
        ) -> Result<(Option<Color>, Vec<Vertex>, Vec<DrawItem>), RendererError> {
            let mut clear = None;
            let mut vertices = Vec::new();
            let mut draws = Vec::new();
            let full_scissor = RECT {
                left: 0,
                top: 0,
                right: self.surface_width.max(1),
                bottom: self.surface_height.max(1),
            };

            let commands = self.recorded_commands.clone();
            for command in &commands {
                match command {
                    FrameCommand::Clear { color } => clear = Some(*color),
                    FrameCommand::FillRect { rect, color } => {
                        push_rect_vertices(
                            &mut vertices,
                            *rect,
                            *color,
                            self.surface_width,
                            self.surface_height,
                        );
                        draws.push(DrawItem {
                            first_vertex: vertices.len() as u32 - 6,
                            vertex_count: 6,
                            pipeline: PipelineKind::Solid,
                            descriptor_index: 0,
                            scissor: full_scissor,
                        });
                    }
                    FrameCommand::StrokeRect {
                        rect,
                        color,
                        thickness,
                    } => {
                        for part in stroke_rect_parts(*rect, *thickness) {
                            if part.width <= 0.0 || part.height <= 0.0 {
                                continue;
                            }
                            push_rect_vertices(
                                &mut vertices,
                                part,
                                *color,
                                self.surface_width,
                                self.surface_height,
                            );
                            draws.push(DrawItem {
                                first_vertex: vertices.len() as u32 - 6,
                                vertex_count: 6,
                                pipeline: PipelineKind::Solid,
                                descriptor_index: 0,
                                scissor: full_scissor,
                            });
                        }
                    }
                    FrameCommand::Image(request) => {
                        let image = self
                            .image_resources
                            .get(&request.image_key)
                            .cloned()
                            .ok_or_else(|| {
                                RendererError::Backend(format!(
                                    "missing image resource '{}'",
                                    request.image_key
                                ))
                            })?;
                        let descriptor_index = self.ensure_texture(&request.image_key, &image)?;
                        push_textured_rect_vertices(
                            &mut vertices,
                            request,
                            self.surface_width,
                            self.surface_height,
                        );
                        draws.push(DrawItem {
                            first_vertex: vertices.len() as u32 - 6,
                            vertex_count: 6,
                            pipeline: PipelineKind::Textured,
                            descriptor_index,
                            scissor: image_scissor(
                                request,
                                self.surface_width,
                                self.surface_height,
                            ),
                        });
                    }
                    _ => {
                        return Err(RendererError::Backend(
                            "DX12 backend received an unsupported command".to_string(),
                        ));
                    }
                }
            }
            Ok((clear, vertices, draws))
        }

        fn draw_frame(&mut self) -> Result<(), RendererError> {
            unsafe {
                self.resize_if_needed()?;
                let (clear_color, vertices, draws) = self.build_draws()?;
                let vertex_bytes = vertices.len() * size_of::<Vertex>();
                self.ensure_vertex_buffer(vertex_bytes.max(size_of::<Vertex>()))?;

                let vertex_buffer = self.vertex_buffer.as_ref().ok_or_else(|| {
                    RendererError::Backend("vertex buffer unavailable".to_string())
                })?;
                let mut mapped = std::ptr::null_mut::<std::ffi::c_void>();
                vertex_buffer.Map(0, None, Some(&mut mapped)).map_err(|err| {
                    RendererError::Backend(format!("vertex buffer map failed: {err}"))
                })?;
                if !vertices.is_empty() {
                    copy_nonoverlapping(
                        vertices.as_ptr() as *const u8,
                        mapped as *mut u8,
                        vertex_bytes,
                    );
                }
                vertex_buffer.Unmap(0, None);

                self.command_allocator.Reset().map_err(|err| {
                    RendererError::Backend(format!("command allocator reset failed: {err}"))
                })?;
                self.command_list
                    .Reset(&self.command_allocator, None)
                    .map_err(|err| {
                        RendererError::Backend(format!("command list reset failed: {err}"))
                    })?;

                let current = self
                    .back_buffers
                    .get(self.frame_index as usize)
                    .cloned()
                    .ok_or_else(|| {
                        RendererError::Backend("back buffer unavailable".to_string())
                    })?;
                self.command_list.ResourceBarrier(&[transition_barrier(
                    &current,
                    D3D12_RESOURCE_STATE_PRESENT,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                )]);

                let rtv = rtv_handle(&self.rtv_heap, self.rtv_descriptor_size, self.frame_index);
                self.command_list
                    .OMSetRenderTargets(1, Some(&rtv), false, None);
                let clear = clear_color.unwrap_or(Color::rgba(0, 0, 0, 255));
                self.command_list.ClearRenderTargetView(
                    rtv,
                    &[
                        clear.r as f32 / 255.0,
                        clear.g as f32 / 255.0,
                        clear.b as f32 / 255.0,
                        clear.a as f32 / 255.0,
                    ],
                    None,
                );

                let heaps = [Some(self.srv_heap.clone())];
                self.command_list.SetDescriptorHeaps(&heaps);
                self.command_list.SetGraphicsRootSignature(&self.root_signature);
                self.command_list.RSSetViewports(&[D3D12_VIEWPORT {
                    TopLeftX: 0.0,
                    TopLeftY: 0.0,
                    Width: self.surface_width.max(1) as f32,
                    Height: self.surface_height.max(1) as f32,
                    MinDepth: 0.0,
                    MaxDepth: 1.0,
                }]);
                self.command_list.RSSetScissorRects(&[RECT {
                    left: 0,
                    top: 0,
                    right: self.surface_width.max(1),
                    bottom: self.surface_height.max(1),
                }]);
                self.command_list
                    .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                self.command_list.IASetVertexBuffers(
                    0,
                    Some(&[D3D12_VERTEX_BUFFER_VIEW {
                        BufferLocation: vertex_buffer.GetGPUVirtualAddress(),
                        SizeInBytes: vertex_bytes.max(size_of::<Vertex>()) as u32,
                        StrideInBytes: size_of::<Vertex>() as u32,
                    }]),
                );

                for draw in &draws {
                    match draw.pipeline {
                        PipelineKind::Solid => {
                            self.command_list.SetPipelineState(&self.solid_pipeline)
                        }
                        PipelineKind::Textured => {
                            self.command_list.SetPipelineState(&self.textured_pipeline);
                            self.command_list.SetGraphicsRootDescriptorTable(
                                0,
                                srv_gpu_handle(
                                    &self.srv_heap,
                                    self.srv_descriptor_size,
                                    draw.descriptor_index,
                                ),
                            );
                        }
                    }
                    self.command_list.RSSetScissorRects(&[draw.scissor]);
                    self.command_list
                        .DrawInstanced(draw.vertex_count, 1, draw.first_vertex, 0);
                }

                self.command_list.ResourceBarrier(&[transition_barrier(
                    &current,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                    D3D12_RESOURCE_STATE_PRESENT,
                )]);
                self.command_list.Close().map_err(|err| {
                    RendererError::Backend(format!("command list close failed: {err}"))
                })?;
                let list: ID3D12CommandList = self.command_list.cast().map_err(|err| {
                    RendererError::Backend(format!("command list cast failed: {err}"))
                })?;
                self.command_queue.ExecuteCommandLists(&[Some(list)]);
                self.swap_chain
                    .Present(1, DXGI_PRESENT(0))
                    .ok()
                    .map_err(|err| {
                    RendererError::Backend(format!("Present failed: {err}"))
                })?;
                self.wait_for_gpu()
            }
        }
    }

    fn create_root_signature(device: &ID3D12Device) -> Result<ID3D12RootSignature, RendererError> {
        unsafe {
            let descriptor_range = D3D12_DESCRIPTOR_RANGE {
                RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
                NumDescriptors: 1,
                BaseShaderRegister: 0,
                RegisterSpace: 0,
                OffsetInDescriptorsFromTableStart: D3D12_DESCRIPTOR_RANGE_OFFSET_APPEND,
            };
            let root_param = D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                        NumDescriptorRanges: 1,
                        pDescriptorRanges: &descriptor_range,
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
            };
            let static_sampler = D3D12_STATIC_SAMPLER_DESC {
                Filter: D3D12_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                MipLODBias: 0.0,
                MaxAnisotropy: 1,
                ComparisonFunc: D3D12_COMPARISON_FUNC_ALWAYS,
                BorderColor: D3D12_STATIC_BORDER_COLOR_TRANSPARENT_BLACK,
                MinLOD: 0.0,
                MaxLOD: D3D12_FLOAT32_MAX,
                ShaderRegister: 0,
                RegisterSpace: 0,
                ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
            };
            let root_sig_desc = D3D12_ROOT_SIGNATURE_DESC {
                NumParameters: 1,
                pParameters: &root_param,
                NumStaticSamplers: 1,
                pStaticSamplers: &static_sampler,
                Flags: D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT,
            };
            let mut blob = None;
            let mut error_blob = None;
            D3D12SerializeRootSignature(
                &root_sig_desc,
                D3D_ROOT_SIGNATURE_VERSION_1,
                &mut blob,
                Some(&mut error_blob),
            )
            .map_err(|err| {
                RendererError::Backend(format!(
                    "D3D12SerializeRootSignature failed: {err}"
                ))
            })?;
            let blob = blob.ok_or_else(|| {
                RendererError::Backend("root signature blob missing".to_string())
            })?;
            device
                .CreateRootSignature(
                    0,
                    std::slice::from_raw_parts(
                        blob.GetBufferPointer() as *const u8,
                        blob.GetBufferSize(),
                    ),
                )
                .map_err(|err| {
                    RendererError::Backend(format!("CreateRootSignature failed: {err}"))
                })
        }
    }

    fn create_pipeline_state(
        device: &ID3D12Device,
        root_signature: &ID3D12RootSignature,
        textured: bool,
    ) -> Result<ID3D12PipelineState, RendererError> {
        unsafe {
            let vs = compile_shader(VERTEX_SHADER, "vs_main", "vs_5_0")?;
            let ps = compile_shader(
                if textured {
                    PIXEL_SHADER_TEXTURED
                } else {
                    PIXEL_SHADER_SOLID
                },
                "ps_main",
                "ps_5_0",
            )?;
            let semantics = [s!("POSITION"), s!("TEXCOORD"), s!("COLOR")];
            let input_layouts = [
                D3D12_INPUT_ELEMENT_DESC {
                    SemanticName: PCSTR(semantics[0].as_ptr() as *const u8),
                    SemanticIndex: 0,
                    Format: DXGI_FORMAT_R32G32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: 0,
                    InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
                D3D12_INPUT_ELEMENT_DESC {
                    SemanticName: PCSTR(semantics[1].as_ptr() as *const u8),
                    SemanticIndex: 0,
                    Format: DXGI_FORMAT_R32G32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: 8,
                    InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
                D3D12_INPUT_ELEMENT_DESC {
                    SemanticName: PCSTR(semantics[2].as_ptr() as *const u8),
                    SemanticIndex: 0,
                    Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: 16,
                    InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
            ];
            let desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                pRootSignature: ManuallyDrop::new(Some(root_signature.clone())),
                VS: shader_bytecode(&vs),
                PS: shader_bytecode(&ps),
                BlendState: alpha_blend_state(),
                SampleMask: u32::MAX,
                RasterizerState: rasterizer_state(),
                DepthStencilState: depth_stencil_state(),
                InputLayout: D3D12_INPUT_LAYOUT_DESC {
                    pInputElementDescs: input_layouts.as_ptr(),
                    NumElements: input_layouts.len() as u32,
                },
                PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
                NumRenderTargets: 1,
                RTVFormats: [
                    DXGI_FORMAT_R8G8B8A8_UNORM,
                    DXGI_FORMAT_UNKNOWN,
                    DXGI_FORMAT_UNKNOWN,
                    DXGI_FORMAT_UNKNOWN,
                    DXGI_FORMAT_UNKNOWN,
                    DXGI_FORMAT_UNKNOWN,
                    DXGI_FORMAT_UNKNOWN,
                    DXGI_FORMAT_UNKNOWN,
                ],
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                ..Default::default()
            };
            device.CreateGraphicsPipelineState(&desc).map_err(|err| {
                RendererError::Backend(format!(
                    "CreateGraphicsPipelineState failed: {err}"
                ))
            })
        }
    }

    fn compile_shader(source: &str, entry: &str, target: &str) -> Result<ID3DBlob, RendererError> {
        unsafe {
            let entry = CString::new(entry).map_err(|err| RendererError::Backend(err.to_string()))?;
            let target = CString::new(target).map_err(|err| RendererError::Backend(err.to_string()))?;
            let source = CString::new(source).map_err(|err| RendererError::Backend(err.to_string()))?;
            let mut blob = None;
            let mut error_blob = None;
            D3DCompile(
                source.as_ptr() as _,
                source.as_bytes().len(),
                PCSTR::null(),
                None,
                None,
                PCSTR(entry.as_ptr() as *const u8),
                PCSTR(target.as_ptr() as *const u8),
                0,
                0,
                &mut blob,
                Some(&mut error_blob),
            )
            .map_err(|err| {
                let detail = error_blob
                    .map(|value| {
                        String::from_utf8_lossy(std::slice::from_raw_parts(
                            value.GetBufferPointer() as *const u8,
                            value.GetBufferSize(),
                        ))
                        .into_owned()
                    })
                    .unwrap_or_default();
                RendererError::Backend(format!("D3DCompile failed: {err}; {detail}"))
            })?;
            blob.ok_or_else(|| RendererError::Backend("shader blob missing".to_string()))
        }
    }

    fn shader_bytecode(blob: &ID3DBlob) -> D3D12_SHADER_BYTECODE {
        unsafe {
            D3D12_SHADER_BYTECODE {
                pShaderBytecode: blob.GetBufferPointer(),
                BytecodeLength: blob.GetBufferSize(),
            }
        }
    }

    fn alpha_blend_state() -> D3D12_BLEND_DESC {
        let target = D3D12_RENDER_TARGET_BLEND_DESC {
            BlendEnable: true.into(),
            LogicOpEnable: false.into(),
            SrcBlend: D3D12_BLEND_SRC_ALPHA,
            DestBlend: D3D12_BLEND_INV_SRC_ALPHA,
            BlendOp: D3D12_BLEND_OP_ADD,
            SrcBlendAlpha: D3D12_BLEND_ONE,
            DestBlendAlpha: D3D12_BLEND_INV_SRC_ALPHA,
            BlendOpAlpha: D3D12_BLEND_OP_ADD,
            LogicOp: D3D12_LOGIC_OP_NOOP,
            RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8,
        };
        D3D12_BLEND_DESC {
            AlphaToCoverageEnable: false.into(),
            IndependentBlendEnable: false.into(),
            RenderTarget: [target; 8],
        }
    }

    fn rasterizer_state() -> D3D12_RASTERIZER_DESC {
        D3D12_RASTERIZER_DESC {
            FillMode: D3D12_FILL_MODE_SOLID,
            CullMode: D3D12_CULL_MODE_NONE,
            FrontCounterClockwise: false.into(),
            DepthBias: D3D12_DEFAULT_DEPTH_BIAS as i32,
            DepthBiasClamp: D3D12_DEFAULT_DEPTH_BIAS_CLAMP,
            SlopeScaledDepthBias: D3D12_DEFAULT_SLOPE_SCALED_DEPTH_BIAS,
            DepthClipEnable: true.into(),
            MultisampleEnable: false.into(),
            AntialiasedLineEnable: false.into(),
            ForcedSampleCount: 0,
            ConservativeRaster: D3D12_CONSERVATIVE_RASTERIZATION_MODE_OFF,
        }
    }

    fn depth_stencil_state() -> D3D12_DEPTH_STENCIL_DESC {
        D3D12_DEPTH_STENCIL_DESC {
            DepthEnable: false.into(),
            StencilEnable: false.into(),
            ..Default::default()
        }
    }

    fn transition_barrier(
        resource: &ID3D12Resource,
        before: D3D12_RESOURCE_STATES,
        after: D3D12_RESOURCE_STATES,
    ) -> D3D12_RESOURCE_BARRIER {
        D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: ManuallyDrop::new(Some(resource.clone())),
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: before,
                    StateAfter: after,
                }),
            },
        }
    }

    fn heap_properties(kind: D3D12_HEAP_TYPE) -> D3D12_HEAP_PROPERTIES {
        D3D12_HEAP_PROPERTIES {
            Type: kind,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 0,
            VisibleNodeMask: 0,
        }
    }

    fn rtv_handle(
        heap: &ID3D12DescriptorHeap,
        increment: u32,
        index: u32,
    ) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        let base = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
        D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: base.ptr + index as usize * increment as usize,
        }
    }

    fn srv_cpu_handle(
        heap: &ID3D12DescriptorHeap,
        increment: u32,
        index: u32,
    ) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        let base = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
        D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: base.ptr + index as usize * increment as usize,
        }
    }

    fn srv_gpu_handle(
        heap: &ID3D12DescriptorHeap,
        increment: u32,
        index: u32,
    ) -> D3D12_GPU_DESCRIPTOR_HANDLE {
        let base = unsafe { heap.GetGPUDescriptorHandleForHeapStart() };
        D3D12_GPU_DESCRIPTOR_HANDLE {
            ptr: base.ptr + index as u64 * increment as u64,
        }
    }

    fn image_hash(image: &DecodedImage) -> [u8; 32] {
        let mut hasher = Hasher::new();
        hasher.update(&image.width.to_le_bytes());
        hasher.update(&image.height.to_le_bytes());
        hasher.update(&image.rgba8);
        *hasher.finalize().as_bytes()
    }

    fn image_scissor(request: &ImageRequest, width: i32, height: i32) -> RECT {
        let clip = request.clip_rect.unwrap_or(request.rect);
        RECT {
            left: clip.x.floor().max(0.0) as i32,
            top: clip.y.floor().max(0.0) as i32,
            right: (clip.x + clip.width).ceil().min(width as f32) as i32,
            bottom: (clip.y + clip.height).ceil().min(height as f32) as i32,
        }
    }

    fn stroke_rect_parts(rect: Rect, thickness: i32) -> [Rect; 4] {
        let t = thickness.max(1) as f32;
        [
            Rect { x: rect.x, y: rect.y, width: rect.width, height: t },
            Rect {
                x: rect.x,
                y: rect.y + rect.height - t,
                width: rect.width,
                height: t,
            },
            Rect {
                x: rect.x,
                y: rect.y + t,
                width: t,
                height: (rect.height - 2.0 * t).max(0.0),
            },
            Rect {
                x: rect.x + rect.width - t,
                y: rect.y + t,
                width: t,
                height: (rect.height - 2.0 * t).max(0.0),
            },
        ]
    }

    fn push_rect_vertices(
        vertices: &mut Vec<Vertex>,
        rect: Rect,
        color: Color,
        surface_width: i32,
        surface_height: i32,
    ) {
        let rgba = [
            color.r as f32 / 255.0,
            color.g as f32 / 255.0,
            color.b as f32 / 255.0,
            color.a as f32 / 255.0,
        ];
        push_quad_vertices(
            vertices,
            rect.x,
            rect.y,
            rect.x + rect.width,
            rect.y + rect.height,
            rgba,
            surface_width,
            surface_height,
            false,
        );
    }

    fn push_textured_rect_vertices(
        vertices: &mut Vec<Vertex>,
        request: &ImageRequest,
        surface_width: i32,
        surface_height: i32,
    ) {
        push_quad_vertices(
            vertices,
            request.rect.x,
            request.rect.y,
            request.rect.x + request.rect.width,
            request.rect.y + request.rect.height,
            [1.0, 1.0, 1.0, request.alpha.clamp(0.0, 1.0)],
            surface_width,
            surface_height,
            true,
        );
    }

    fn push_quad_vertices(
        vertices: &mut Vec<Vertex>,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        color: [f32; 4],
        surface_width: i32,
        surface_height: i32,
        textured: bool,
    ) {
        let left = (x0 / surface_width.max(1) as f32) * 2.0 - 1.0;
        let right = (x1 / surface_width.max(1) as f32) * 2.0 - 1.0;
        let top = 1.0 - (y0 / surface_height.max(1) as f32) * 2.0;
        let bottom = 1.0 - (y1 / surface_height.max(1) as f32) * 2.0;
        let (uv0, uv1) = if textured { ([0.0, 0.0], [1.0, 1.0]) } else { ([0.0, 0.0], [0.0, 0.0]) };
        vertices.extend_from_slice(&[
            Vertex { position: [left, top], uv: [uv0[0], uv0[1]], color },
            Vertex { position: [right, bottom], uv: [uv1[0], uv1[1]], color },
            Vertex { position: [left, bottom], uv: [uv0[0], uv1[1]], color },
            Vertex { position: [left, top], uv: [uv0[0], uv0[1]], color },
            Vertex { position: [right, top], uv: [uv1[0], uv0[1]], color },
            Vertex { position: [right, bottom], uv: [uv1[0], uv1[1]], color },
        ]);
    }

    const VERTEX_SHADER: &str = r#"
struct VSInput { float2 pos : POSITION; float2 uv : TEXCOORD; float4 color : COLOR; };
struct VSOutput { float4 pos : SV_POSITION; float2 uv : TEXCOORD; float4 color : COLOR; };
VSOutput vs_main(VSInput input) {
    VSOutput output;
    output.pos = float4(input.pos, 0.0, 1.0);
    output.uv = input.uv;
    output.color = input.color;
    return output;
}"#;

    const PIXEL_SHADER_SOLID: &str = r#"
struct PSInput { float4 pos : SV_POSITION; float2 uv : TEXCOORD; float4 color : COLOR; };
float4 ps_main(PSInput input) : SV_TARGET { return input.color; }"#;

    const PIXEL_SHADER_TEXTURED: &str = r#"
Texture2D tex0 : register(t0);
SamplerState samp0 : register(s0);
struct PSInput { float4 pos : SV_POSITION; float2 uv : TEXCOORD; float4 color : COLOR; };
float4 ps_main(PSInput input) : SV_TARGET { return tex0.Sample(samp0, input.uv) * input.color; }"#;
}

#[cfg(target_os = "windows")]
pub use windows_backend::{Dx12Backend, Dx12BackendState};

#[cfg(not(target_os = "windows"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dx12BackendState {
    UnboundSurface,
}

#[cfg(not(target_os = "windows"))]
pub struct Dx12Backend {
    recorded_commands: Vec<FrameCommand>,
    image_resources: HashMap<String, Arc<DecodedImage>>,
}

#[cfg(not(target_os = "windows"))]
impl Dx12Backend {
    pub fn try_bind_hwnd(_hwnd: isize, _width: i32, _height: i32) -> Result<Self, RendererError> {
        Err(RendererError::Backend(
            "DX12 backend is only available on Windows".to_string(),
        ))
    }

    pub fn state(&self) -> Dx12BackendState {
        Dx12BackendState::UnboundSurface
    }

    pub fn update_surface_size(&mut self, _width: i32, _height: i32) {}

    pub fn supports_commands(&self, _commands: &[FrameCommand]) -> bool {
        false
    }

    pub fn sync_image_resources(
        &mut self,
        resources: impl IntoIterator<Item = (String, Arc<DecodedImage>)>,
    ) {
        self.image_resources.clear();
        for (key, image) in resources {
            self.image_resources.insert(key, image);
        }
    }
}

#[cfg(not(target_os = "windows"))]
impl GraphicsBackend for Dx12Backend {
    fn begin_frame(&mut self) -> Result<(), RendererError> {
        self.recorded_commands.clear();
        Ok(())
    }

    fn submit(&mut self, commands: &[FrameCommand]) -> Result<(), RendererError> {
        self.recorded_commands.extend_from_slice(commands);
        Ok(())
    }

    fn end_frame(&mut self) -> Result<(), RendererError> {
        Err(RendererError::Backend(
            "DX12 backend is only available on Windows".to_string(),
        ))
    }
}
