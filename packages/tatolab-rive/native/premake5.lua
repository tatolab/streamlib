-- Copyright (c) 2025 Jonathan Fontanez
-- SPDX-License-Identifier: GPL-3.0-only

-- rive-runtime's runtime and Vulkan renderer, and this wheel's bridge, in one
-- workspace so the bridge compiles against the same class layouts the
-- libraries were built with. Run through rive-runtime's `build_rive.sh`, with
-- TATOLAB_RIVE_RUNTIME_DIRECTORY naming the checkout.

local rive_runtime_directory = os.getenv('TATOLAB_RIVE_RUNTIME_DIRECTORY')
if rive_runtime_directory == nil or rive_runtime_directory == '' then
    error('TATOLAB_RIVE_RUNTIME_DIRECTORY names no rive-runtime checkout')
end

dofile('rive_build_config.lua')
dofile(rive_runtime_directory .. '/renderer/premake5_pls_renderer.lua')
dofile(RIVE_RUNTIME_DIR .. '/premake5_v2.lua')
dofile(RIVE_RUNTIME_DIR .. '/decoders/premake5_v2.lua')

project('tatolab_rive_external_renderer_bridge')
do
    dependson({ 'rive', 'rive_pls_renderer' })
    kind('StaticLib')
    includedirs({
        RIVE_RUNTIME_DIR .. '/include',
        RIVE_RUNTIME_DIR .. '/renderer/include',
    })
    externalincludedirs({
        vulkan_headers .. '/include',
        vulkan_memory_allocator .. '/include',
        yoga,
    })
    defines({ 'YOGA_EXPORT=' })
    files({ 'tatolab_rive_external_renderer_bridge.cpp' })
end
