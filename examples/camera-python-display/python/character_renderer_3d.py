# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""3D Character Renderer with GPU Skinning.

Renders a rigged GLB character driven by MediaPipe pose landmarks.
Uses ModernGL for GPU-accelerated skinned mesh rendering.

Architecture:
- GLBLoader: Extracts mesh, skeleton, and skinning data from glTF/GLB files
- SkinnedMeshRenderer: GPU skinning via vertex shader
- PoseSolver: Converts MediaPipe landmarks to bone rotations
- Scene3D: Manages camera, lighting, and scene objects (supports backgrounds)
"""

import json
import logging
import math
import struct
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

import moderngl
import numpy as np
from pyrr import Matrix44, Quaternion, Vector3

logger = logging.getLogger(__name__)

# Maximum bones supported by GPU skinning shader
MAX_BONES = 64


# =============================================================================
# Shader Sources
# =============================================================================

SKINNED_VERTEX_SHADER = """
#version 330 core

// Vertex attributes
in vec3 in_position;
in vec3 in_normal;
in vec2 in_texcoord;
in vec4 in_joints;   // Bone indices (up to 4 bones per vertex)
in vec4 in_weights;  // Bone weights (must sum to 1.0)

// Uniforms
uniform mat4 u_model;
uniform mat4 u_view;
uniform mat4 u_projection;
uniform mat4 u_bone_matrices[64];  // Bone transformation matrices

// Outputs to fragment shader
out vec3 v_normal;
out vec2 v_texcoord;
out vec3 v_position;

void main() {
    // GPU Skinning: blend vertex position by bone weights
    mat4 skin_matrix = mat4(0.0);

    // Accumulate weighted bone transforms
    skin_matrix += u_bone_matrices[int(in_joints.x)] * in_weights.x;
    skin_matrix += u_bone_matrices[int(in_joints.y)] * in_weights.y;
    skin_matrix += u_bone_matrices[int(in_joints.z)] * in_weights.z;
    skin_matrix += u_bone_matrices[int(in_joints.w)] * in_weights.w;

    // Apply skinning to position and normal
    vec4 skinned_position = skin_matrix * vec4(in_position, 1.0);
    vec4 skinned_normal = skin_matrix * vec4(in_normal, 0.0);

    // Transform to clip space
    vec4 world_position = u_model * skinned_position;
    gl_Position = u_projection * u_view * world_position;

    // Pass to fragment shader
    v_position = world_position.xyz;
    v_normal = normalize((u_model * skinned_normal).xyz);
    v_texcoord = in_texcoord;
}
"""

FRAGMENT_SHADER = """
#version 330 core

in vec3 v_normal;
in vec2 v_texcoord;
in vec3 v_position;

// Material uniforms
uniform vec4 u_base_color;
uniform float u_metallic;
uniform float u_roughness;
uniform sampler2D u_texture;
uniform bool u_has_texture;

// Lighting
uniform vec3 u_light_dir;
uniform vec3 u_light_color;
uniform vec3 u_ambient;
uniform vec3 u_camera_pos;

out vec4 frag_color;

// Fresnel-Schlick approximation
vec3 fresnelSchlick(float cosTheta, vec3 F0) {
    return F0 + (1.0 - F0) * pow(clamp(1.0 - cosTheta, 0.0, 1.0), 5.0);
}

void main() {
    vec3 normal = normalize(v_normal);
    vec3 light_dir = normalize(u_light_dir);
    vec3 view_dir = normalize(u_camera_pos - v_position);
    vec3 halfway = normalize(light_dir + view_dir);

    // Base color (from texture or uniform)
    vec4 base = u_has_texture ? texture(u_texture, v_texcoord) : u_base_color;
    vec3 albedo = base.rgb;

    // Metallic workflow - metals tint reflections with albedo
    vec3 F0 = mix(vec3(0.04), albedo, u_metallic);

    // Fresnel
    float NdotV = max(dot(normal, view_dir), 0.0);
    vec3 fresnel = fresnelSchlick(NdotV, F0);

    // Diffuse (reduced by metallic - metals don't have diffuse)
    float NdotL = max(dot(normal, light_dir), 0.0);
    vec3 diffuse = (1.0 - u_metallic) * albedo * NdotL * u_light_color;

    // Specular (Blinn-Phong with roughness)
    float NdotH = max(dot(normal, halfway), 0.0);
    float shininess = mix(8.0, 256.0, 1.0 - u_roughness);
    float spec = pow(NdotH, shininess);
    vec3 specular = fresnel * spec * u_light_color * (1.0 - u_roughness * 0.5);

    // Ambient with metallic tint
    vec3 ambient = u_ambient * mix(albedo, F0, u_metallic * 0.5);

    // Rim lighting (cyberpunk edge glow)
    float rim = 1.0 - NdotV;
    rim = smoothstep(0.4, 1.0, rim);
    vec3 rim_color = vec3(0.0, 0.7, 0.9) * rim * 0.4; // Cyan rim

    // Emissive for bright colors (cyan, yellow glow)
    float brightness = dot(albedo, vec3(0.299, 0.587, 0.114));
    float isCyan = step(0.8, albedo.g) * step(0.8, albedo.b) * step(albedo.r, 0.3);
    float isYellow = step(0.8, albedo.r) * step(0.8, albedo.g) * step(albedo.b, 0.3);
    float isOrange = step(0.8, albedo.r) * step(0.3, albedo.g) * step(albedo.b, 0.3);
    float emissive_strength = (isCyan + isYellow * 0.8 + isOrange * 0.6) * 0.5;
    vec3 emissive = albedo * emissive_strength;

    // Combine
    vec3 result = ambient + diffuse + specular + rim_color + emissive;

    // Slight tone mapping
    result = result / (result + vec3(0.5));

    frag_color = vec4(result, base.a);
}
"""

# Simple background shader (for 2D backdrop or skybox)
BACKGROUND_VERTEX_SHADER = """
#version 330 core
in vec2 in_position;
in vec2 in_texcoord;
out vec2 v_texcoord;

void main() {
    gl_Position = vec4(in_position, 0.999, 1.0);  // Far depth
    v_texcoord = in_texcoord;
}
"""

BACKGROUND_FRAGMENT_SHADER = """
#version 330 core
in vec2 v_texcoord;
uniform sampler2D u_texture;
out vec4 frag_color;

void main() {
    frag_color = texture(u_texture, v_texcoord);
}
"""

# Cyberpunk bloom post-processing shaders
FULLSCREEN_VERTEX_SHADER = """
#version 330 core
in vec2 in_position;
out vec2 v_texcoord;

void main() {
    gl_Position = vec4(in_position, 0.0, 1.0);
    v_texcoord = in_position * 0.5 + 0.5;
}
"""

BLOOM_EXTRACT_SHADER = """
#version 330 core
in vec2 v_texcoord;
uniform sampler2D u_texture;
uniform float u_threshold;
out vec4 frag_color;

void main() {
    vec4 color = texture(u_texture, v_texcoord);
    float brightness = dot(color.rgb, vec3(0.2126, 0.7152, 0.0722));
    if (brightness > u_threshold) {
        frag_color = color;
    } else {
        frag_color = vec4(0.0);
    }
}
"""

BLUR_SHADER = """
#version 330 core
in vec2 v_texcoord;
uniform sampler2D u_texture;
uniform vec2 u_direction;  // (1/width, 0) or (0, 1/height)
out vec4 frag_color;

void main() {
    vec4 result = vec4(0.0);
    // 9-tap Gaussian blur
    float weights[5] = float[](0.227027, 0.1945946, 0.1216216, 0.054054, 0.016216);

    result += texture(u_texture, v_texcoord) * weights[0];
    for (int i = 1; i < 5; i++) {
        vec2 offset = u_direction * float(i) * 2.0;
        result += texture(u_texture, v_texcoord + offset) * weights[i];
        result += texture(u_texture, v_texcoord - offset) * weights[i];
    }
    frag_color = result;
}
"""

BLOOM_COMPOSITE_SHADER = """
#version 330 core
in vec2 v_texcoord;
uniform sampler2D u_scene;
uniform sampler2D u_bloom;
uniform float u_bloom_intensity;
uniform vec3 u_tint;  // Cyberpunk color tint
out vec4 frag_color;

void main() {
    vec4 scene = texture(u_scene, v_texcoord);
    vec4 bloom = texture(u_bloom, v_texcoord);

    // Add cyberpunk color tint to bloom
    vec3 tinted_bloom = bloom.rgb * u_tint;

    // Combine with scene
    vec3 result = scene.rgb + tinted_bloom * u_bloom_intensity;

    // Slight tone mapping to prevent blowout
    result = result / (result + vec3(1.0));

    // Add subtle vignette
    vec2 uv = v_texcoord * 2.0 - 1.0;
    float vignette = 1.0 - dot(uv, uv) * 0.15;
    result *= vignette;

    frag_color = vec4(result, scene.a);
}
"""


# =============================================================================
# Data Classes
# =============================================================================

@dataclass
class Bone:
    """Skeleton bone with transform hierarchy."""
    name: str
    index: int
    parent_index: int = -1
    inverse_bind_matrix: np.ndarray = field(default_factory=lambda: np.eye(4, dtype=np.float32))
    local_transform: np.ndarray = field(default_factory=lambda: np.eye(4, dtype=np.float32))
    world_transform: np.ndarray = field(default_factory=lambda: np.eye(4, dtype=np.float32))


@dataclass
class Primitive:
    """A mesh primitive (submesh) with its own material."""
    vao: moderngl.VertexArray
    vertex_count: int
    material_index: int


@dataclass
class Material:
    """PBR material properties."""
    name: str
    base_color: tuple = (0.8, 0.8, 0.8, 1.0)
    metallic: float = 0.0
    roughness: float = 0.5
    texture: Optional[moderngl.Texture] = None


# =============================================================================
# GLB Loader
# =============================================================================

class GLBLoader:
    """Loads glTF/GLB files and extracts mesh + skeleton data."""

    def __init__(self, ctx: moderngl.Context):
        self.ctx = ctx
        self.gltf_json = None
        self.binary_data = None

    def load(self, path: Path) -> tuple[list[Primitive], list[Bone], list[Material]]:
        """Load GLB file and return primitives, bones, and materials."""
        with open(path, 'rb') as f:
            # Parse GLB header
            magic = f.read(4)
            if magic != b'glTF':
                raise ValueError("Not a valid GLB file")

            version = struct.unpack('<I', f.read(4))[0]
            total_length = struct.unpack('<I', f.read(4))[0]

            # Read JSON chunk
            chunk_length = struct.unpack('<I', f.read(4))[0]
            chunk_type = f.read(4)
            self.gltf_json = json.loads(f.read(chunk_length).decode('utf-8'))

            # Read binary chunk
            if f.tell() < total_length:
                chunk_length = struct.unpack('<I', f.read(4))[0]
                chunk_type = f.read(4)
                self.binary_data = f.read(chunk_length)

        # Extract data
        materials = self._load_materials()
        bones = self._load_skeleton()
        primitives = self._load_mesh(bones)

        return primitives, bones, materials

    def _get_accessor_data(self, accessor_index: int) -> np.ndarray:
        """Extract numpy array from accessor."""
        accessor = self.gltf_json['accessors'][accessor_index]
        buffer_view = self.gltf_json['bufferViews'][accessor['bufferView']]

        # Determine dtype
        component_type = accessor['componentType']
        dtype_map = {
            5120: np.int8, 5121: np.uint8,
            5122: np.int16, 5123: np.uint16,
            5125: np.uint32, 5126: np.float32,
        }
        dtype = dtype_map[component_type]

        # Determine shape
        type_sizes = {'SCALAR': 1, 'VEC2': 2, 'VEC3': 3, 'VEC4': 4, 'MAT4': 16}
        components = type_sizes[accessor['type']]
        count = accessor['count']

        # Extract data
        offset = buffer_view.get('byteOffset', 0) + accessor.get('byteOffset', 0)
        byte_length = count * components * np.dtype(dtype).itemsize
        data = np.frombuffer(self.binary_data[offset:offset + byte_length], dtype=dtype)

        if components > 1:
            data = data.reshape(count, components)

        return data

    def _load_texture(self, texture_index: int) -> Optional[moderngl.Texture]:
        """Load a texture from GLB data."""
        try:
            textures = self.gltf_json.get('textures', [])
            if texture_index >= len(textures):
                return None

            texture_info = textures[texture_index]
            source_index = texture_info.get('source')
            if source_index is None:
                return None

            images = self.gltf_json.get('images', [])
            if source_index >= len(images):
                return None

            image_info = images[source_index]

            # Get image data from buffer view
            buffer_view_index = image_info.get('bufferView')
            if buffer_view_index is None:
                return None

            buffer_view = self.gltf_json['bufferViews'][buffer_view_index]
            offset = buffer_view.get('byteOffset', 0)
            length = buffer_view['byteLength']

            image_data = self.binary_data[offset:offset + length]

            # Decode image using PIL
            from PIL import Image
            import io

            img = Image.open(io.BytesIO(image_data))
            img = img.convert('RGBA')
            img = img.transpose(Image.FLIP_TOP_BOTTOM)  # OpenGL expects flipped

            # Create ModernGL texture
            texture = self.ctx.texture(img.size, 4, img.tobytes())
            texture.filter = (moderngl.LINEAR_MIPMAP_LINEAR, moderngl.LINEAR)
            texture.build_mipmaps()

            logger.info(f"Loaded texture: {img.size[0]}x{img.size[1]}")
            return texture

        except Exception as e:
            logger.warning(f"Failed to load texture {texture_index}: {e}")
            return None

    def _load_materials(self) -> list[Material]:
        """Load materials from glTF with cyberpunk color scheme."""
        materials = []

        # Cyberpunk color palette (David Martinez / Edgerunners inspired)
        CYBER_CYAN = (0.0, 0.94, 1.0, 1.0)       # #00f0ff - iconic cyan
        CYBER_YELLOW = (0.988, 0.933, 0.039, 1.0) # #fcee0a - accent yellow
        CYBER_ORANGE = (1.0, 0.4, 0.1, 1.0)       # Hot orange
        CYBER_DARK = (0.08, 0.08, 0.1, 1.0)       # Near-black with blue tint
        CYBER_CHROME = (0.7, 0.75, 0.8, 1.0)      # Chrome/silver
        CYBER_DARK_CHROME = (0.25, 0.27, 0.3, 1.0) # Dark metallic
        CYBER_RED = (0.9, 0.1, 0.2, 1.0)          # Warning red

        for i, mat_data in enumerate(self.gltf_json.get('materials', [])):
            pbr = mat_data.get('pbrMetallicRoughness', {})
            name = mat_data.get('name', f'material_{i}')

            # Try to load base color texture first
            texture = None
            if 'baseColorTexture' in pbr:
                tex_info = pbr['baseColorTexture']
                texture = self._load_texture(tex_info.get('index', 0))

            # If no texture, apply cyberpunk color scheme
            if texture is None:
                # Assign colors based on material index for variety
                # This creates a robot-humanoid look like David Martinez
                if i == 0:
                    # Main body - dark metallic
                    base_color = CYBER_DARK
                    metallic = 0.8
                    roughness = 0.3
                elif i == 1:
                    # Secondary body - dark chrome
                    base_color = CYBER_DARK_CHROME
                    metallic = 0.9
                    roughness = 0.2
                elif i == 2:
                    # Accent piece 1 - cyan glow
                    base_color = CYBER_CYAN
                    metallic = 0.5
                    roughness = 0.4
                elif i == 3:
                    # Chrome highlights
                    base_color = CYBER_CHROME
                    metallic = 1.0
                    roughness = 0.1
                elif i == 4:
                    # Dark panel
                    base_color = CYBER_DARK
                    metallic = 0.7
                    roughness = 0.4
                elif i == 5:
                    # Yellow accent (like David's jacket trim)
                    base_color = CYBER_YELLOW
                    metallic = 0.3
                    roughness = 0.5
                elif i == 6:
                    # Another cyan accent
                    base_color = CYBER_CYAN
                    metallic = 0.6
                    roughness = 0.3
                elif i == 7:
                    # Dark chrome panel
                    base_color = CYBER_DARK_CHROME
                    metallic = 0.85
                    roughness = 0.25
                elif i == 8:
                    # Orange accent (energy/heat)
                    base_color = CYBER_ORANGE
                    metallic = 0.4
                    roughness = 0.5
                elif i == 9:
                    # Chrome detail
                    base_color = CYBER_CHROME
                    metallic = 0.95
                    roughness = 0.15
                elif i == 10:
                    # Red warning accent
                    base_color = CYBER_RED
                    metallic = 0.5
                    roughness = 0.4
                else:
                    # Default - dark metallic
                    base_color = CYBER_DARK_CHROME
                    metallic = 0.8
                    roughness = 0.3

                logger.info(f"Material '{name}' -> cyberpunk color {base_color[:3]}")
            else:
                base_color = tuple(pbr.get('baseColorFactor', [0.8, 0.8, 0.8, 1.0]))
                metallic = pbr.get('metallicFactor', 0.0)
                roughness = pbr.get('roughnessFactor', 0.5)

            material = Material(
                name=name,
                base_color=base_color,
                metallic=metallic,
                roughness=roughness,
                texture=texture,
            )
            materials.append(material)

        # Ensure at least one material
        if not materials:
            materials.append(Material(name='default', base_color=CYBER_DARK_CHROME, metallic=0.8, roughness=0.3))

        return materials

    def _load_skeleton(self) -> list[Bone]:
        """Load skeleton/joints from glTF skin."""
        bones = []
        skins = self.gltf_json.get('skins', [])
        if not skins:
            return bones

        skin = skins[0]  # Use first skin
        joints = skin.get('joints', [])
        nodes = self.gltf_json.get('nodes', [])

        # Get inverse bind matrices
        ibm_data = None
        if 'inverseBindMatrices' in skin:
            ibm_data = self._get_accessor_data(skin['inverseBindMatrices'])

        # Build joint index mapping
        joint_to_bone = {j: i for i, j in enumerate(joints)}

        for i, joint_idx in enumerate(joints):
            node = nodes[joint_idx]

            # Get inverse bind matrix
            ibm = np.eye(4, dtype=np.float32)
            if ibm_data is not None:
                ibm = ibm_data[i].reshape(4, 4).T  # Column-major to row-major

            # Get local transform from node
            local = np.eye(4, dtype=np.float32)
            if 'matrix' in node:
                local = np.array(node['matrix'], dtype=np.float32).reshape(4, 4).T
            else:
                # Build from TRS
                T = np.eye(4, dtype=np.float32)
                R = np.eye(4, dtype=np.float32)
                S = np.eye(4, dtype=np.float32)

                if 'translation' in node:
                    T[:3, 3] = node['translation']
                if 'rotation' in node:
                    q = Quaternion(node['rotation'])
                    R[:3, :3] = q.matrix33
                if 'scale' in node:
                    np.fill_diagonal(S[:3, :3], node['scale'])

                local = T @ R @ S

            # Find parent
            parent_index = -1
            for parent_joint_idx, child_indices in self._get_node_children().items():
                if joint_idx in child_indices and parent_joint_idx in joint_to_bone:
                    parent_index = joint_to_bone[parent_joint_idx]
                    break

            bone = Bone(
                name=node.get('name', f'bone_{i}'),
                index=i,
                parent_index=parent_index,
                inverse_bind_matrix=ibm,
                local_transform=local,
            )
            bones.append(bone)

        # Compute initial world transforms
        self._update_bone_world_transforms(bones)

        return bones

    def _get_node_children(self) -> dict[int, list[int]]:
        """Build parent -> children mapping for nodes."""
        children_map = {}
        for i, node in enumerate(self.gltf_json.get('nodes', [])):
            if 'children' in node:
                children_map[i] = node['children']
        return children_map

    def _update_bone_world_transforms(self, bones: list[Bone]):
        """Compute world transforms for all bones."""
        for bone in bones:
            if bone.parent_index >= 0:
                parent = bones[bone.parent_index]
                bone.world_transform = parent.world_transform @ bone.local_transform
            else:
                bone.world_transform = bone.local_transform.copy()

    def _load_mesh(self, bones: list[Bone]) -> list[Primitive]:
        """Load mesh primitives with skinning data."""
        primitives = []
        meshes = self.gltf_json.get('meshes', [])
        if not meshes:
            return primitives

        mesh = meshes[0]  # Use first mesh

        for prim_data in mesh.get('primitives', []):
            attrs = prim_data.get('attributes', {})

            # Required: positions
            if 'POSITION' not in attrs:
                continue

            positions = self._get_accessor_data(attrs['POSITION']).astype(np.float32)
            vertex_count = len(positions)

            # Normals (generate if missing)
            if 'NORMAL' in attrs:
                normals = self._get_accessor_data(attrs['NORMAL']).astype(np.float32)
            else:
                normals = np.zeros_like(positions)
                normals[:, 1] = 1.0  # Default up

            # Texture coordinates
            if 'TEXCOORD_0' in attrs:
                texcoords = self._get_accessor_data(attrs['TEXCOORD_0']).astype(np.float32)
            else:
                texcoords = np.zeros((vertex_count, 2), dtype=np.float32)

            # Skinning: joints and weights
            if 'JOINTS_0' in attrs and 'WEIGHTS_0' in attrs:
                joints = self._get_accessor_data(attrs['JOINTS_0']).astype(np.float32)
                weights = self._get_accessor_data(attrs['WEIGHTS_0']).astype(np.float32)
            else:
                # No skinning - bind to root
                joints = np.zeros((vertex_count, 4), dtype=np.float32)
                weights = np.zeros((vertex_count, 4), dtype=np.float32)
                weights[:, 0] = 1.0

            # Indices
            if 'indices' in prim_data:
                indices = self._get_accessor_data(prim_data['indices']).astype(np.uint32)
            else:
                indices = np.arange(vertex_count, dtype=np.uint32)

            # Create buffers
            vbo_positions = self.ctx.buffer(positions.tobytes())
            vbo_normals = self.ctx.buffer(normals.tobytes())
            vbo_texcoords = self.ctx.buffer(texcoords.tobytes())
            vbo_joints = self.ctx.buffer(joints.tobytes())
            vbo_weights = self.ctx.buffer(weights.tobytes())
            ibo = self.ctx.buffer(indices.tobytes())

            # We'll create VAO later when we have the shader program
            primitives.append({
                'vbo_positions': vbo_positions,
                'vbo_normals': vbo_normals,
                'vbo_texcoords': vbo_texcoords,
                'vbo_joints': vbo_joints,
                'vbo_weights': vbo_weights,
                'ibo': ibo,
                'vertex_count': len(indices),
                'material_index': prim_data.get('material', 0),
            })

        return primitives


# =============================================================================
# Skinned Mesh Renderer
# =============================================================================

class SkinnedMeshRenderer:
    """Renders skinned meshes with GPU acceleration."""

    def __init__(self, ctx: moderngl.Context):
        self.ctx = ctx
        self.program = None
        self.primitives: list[Primitive] = []
        self.bones: list[Bone] = []
        self.materials: list[Material] = []
        self.bone_matrices = np.zeros((MAX_BONES, 4, 4), dtype=np.float32)

        # Initialize identity matrices
        for i in range(MAX_BONES):
            self.bone_matrices[i] = np.eye(4, dtype=np.float32)

        self._create_shader()

    def _create_shader(self):
        """Compile the skinning shader program."""
        self.program = self.ctx.program(
            vertex_shader=SKINNED_VERTEX_SHADER,
            fragment_shader=FRAGMENT_SHADER,
        )

    def load_model(self, path: Path):
        """Load a GLB model."""
        loader = GLBLoader(self.ctx)
        prim_data, self.bones, self.materials = loader.load(path)

        # Create VAOs for each primitive
        self.primitives = []
        for data in prim_data:
            vao = self.ctx.vertex_array(
                self.program,
                [
                    (data['vbo_positions'], '3f', 'in_position'),
                    (data['vbo_normals'], '3f', 'in_normal'),
                    (data['vbo_texcoords'], '2f', 'in_texcoord'),
                    (data['vbo_joints'], '4f', 'in_joints'),
                    (data['vbo_weights'], '4f', 'in_weights'),
                ],
                data['ibo'],
            )
            self.primitives.append(Primitive(
                vao=vao,
                vertex_count=data['vertex_count'],
                material_index=min(data['material_index'], len(self.materials) - 1),
            ))

        logger.info(f"Loaded model: {len(self.primitives)} primitives, {len(self.bones)} bones")

        # Initialize bone matrices to bind pose
        # This must be done after loading, otherwise bone_matrices stays as identity
        for bone in self.bones:
            self.bone_matrices[bone.index] = bone.world_transform @ bone.inverse_bind_matrix

    def update_bone_transforms(self, local_transforms: dict[str, np.ndarray]):
        """Update bone local transforms and recompute world transforms.

        Args:
            local_transforms: Dict mapping bone name to 4x4 local transform matrix
        """
        # Apply new local transforms
        for bone in self.bones:
            if bone.name in local_transforms:
                bone.local_transform = local_transforms[bone.name]

        # Recompute world transforms (must be in parent-first order)
        for bone in self.bones:
            if bone.parent_index >= 0:
                parent = self.bones[bone.parent_index]
                bone.world_transform = parent.world_transform @ bone.local_transform
            else:
                bone.world_transform = bone.local_transform.copy()

        # Compute final bone matrices (world * inverse_bind)
        for bone in self.bones:
            self.bone_matrices[bone.index] = bone.world_transform @ bone.inverse_bind_matrix

    def set_bone_rotations(self, rotations: dict[str, Quaternion]):
        """Set bone rotations while preserving translations.

        Args:
            rotations: Dict mapping bone name to rotation quaternion
        """
        local_transforms = {}

        for bone in self.bones:
            if bone.name in rotations:
                # Preserve translation, replace rotation
                T = np.eye(4, dtype=np.float32)
                T[:3, 3] = bone.local_transform[:3, 3]

                R = np.eye(4, dtype=np.float32)
                R[:3, :3] = rotations[bone.name].matrix33

                local_transforms[bone.name] = T @ R

        if local_transforms:
            self.update_bone_transforms(local_transforms)
            if len(local_transforms) > 0 and not hasattr(self, '_logged_pose'):
                self._logged_pose = True
                logger.info(f"Pose applied to {len(local_transforms)} bones: {list(local_transforms.keys())[:5]}...")

    def render(self, view: np.ndarray, projection: np.ndarray,
               model: np.ndarray = None, camera_pos: np.ndarray = None):
        """Render the skinned mesh.

        Args:
            view: 4x4 view matrix
            projection: 4x4 projection matrix
            model: 4x4 model matrix (optional, defaults to identity)
            camera_pos: Camera position for specular lighting
        """
        if model is None:
            model = np.eye(4, dtype=np.float32)
        if camera_pos is None:
            camera_pos = np.array([0, 0, 5], dtype=np.float32)

        # Write matrices - pyrr matrices are column-major compatible, model needs transpose
        # NumPy row-major -> OpenGL column-major requires transpose
        self.program['u_model'].write(np.ascontiguousarray(model.T, dtype=np.float32).tobytes())
        self.program['u_view'].write(view.astype(np.float32).tobytes())
        self.program['u_projection'].write(projection.astype(np.float32).tobytes())

        # Transpose bone matrices from row-major (NumPy) to column-major (OpenGL)
        # Each 4x4 matrix needs to be transposed before upload
        gpu_bone_matrices = np.zeros((MAX_BONES, 4, 4), dtype=np.float32)
        for i in range(len(self.bones)):
            gpu_bone_matrices[i] = self.bone_matrices[i].T
        # Fill remaining slots with identity (transposed identity = identity)
        for i in range(len(self.bones), MAX_BONES):
            gpu_bone_matrices[i] = np.eye(4, dtype=np.float32)
        self.program['u_bone_matrices'].write(gpu_bone_matrices.tobytes())

        # Interview-style lighting - bright key light from front-right, warm fill
        if 'u_light_dir' in self.program:
            self.program['u_light_dir'].value = (0.4, 0.6, 0.8)  # Front-right key light
        if 'u_light_color' in self.program:
            self.program['u_light_color'].value = (1.2, 1.15, 1.1)  # Bright warm light
        if 'u_ambient' in self.program:
            self.program['u_ambient'].value = (0.35, 0.35, 0.4)  # Stronger ambient fill
        if 'u_camera_pos' in self.program:
            self.program['u_camera_pos'].value = tuple(camera_pos)

        # Render each primitive with its material
        for prim in self.primitives:
            mat = self.materials[prim.material_index]

            # Set uniforms if they exist (driver may optimize out unused ones)
            if 'u_base_color' in self.program:
                self.program['u_base_color'].value = mat.base_color
            if 'u_metallic' in self.program:
                self.program['u_metallic'].value = mat.metallic
            if 'u_roughness' in self.program:
                self.program['u_roughness'].value = mat.roughness
            if 'u_has_texture' in self.program:
                self.program['u_has_texture'].value = mat.texture is not None

            if mat.texture and 'u_texture' in self.program:
                mat.texture.use(0)
                self.program['u_texture'].value = 0

            prim.vao.render(moderngl.TRIANGLES)


# =============================================================================
# Pose Solver - MediaPipe to Bone Rotations
# =============================================================================

class PoseSolver:
    """Converts MediaPipe pose landmarks to bone rotations."""

    # MediaPipe landmark indices
    NOSE = 0
    LEFT_SHOULDER = 11
    RIGHT_SHOULDER = 12
    LEFT_ELBOW = 13
    RIGHT_ELBOW = 14
    LEFT_WRIST = 15
    RIGHT_WRIST = 16
    LEFT_HIP = 23
    RIGHT_HIP = 24
    LEFT_KNEE = 25
    RIGHT_KNEE = 26
    LEFT_ANKLE = 27
    RIGHT_ANKLE = 28

    # Mixamo bone name mapping
    MIXAMO_BONES = {
        'hips': 'mixamorig:Hips',
        'spine': 'mixamorig:Spine',
        'spine1': 'mixamorig:Spine1',
        'spine2': 'mixamorig:Spine2',
        'neck': 'mixamorig:Neck',
        'head': 'mixamorig:Head',
        'left_shoulder': 'mixamorig:LeftShoulder',
        'left_arm': 'mixamorig:LeftArm',
        'left_forearm': 'mixamorig:LeftForeArm',
        'left_hand': 'mixamorig:LeftHand',
        'right_shoulder': 'mixamorig:RightShoulder',
        'right_arm': 'mixamorig:RightArm',
        'right_forearm': 'mixamorig:RightForeArm',
        'right_hand': 'mixamorig:RightHand',
        'left_upleg': 'mixamorig:LeftUpLeg',
        'left_leg': 'mixamorig:LeftLeg',
        'left_foot': 'mixamorig:LeftFoot',
        'right_upleg': 'mixamorig:RightUpLeg',
        'right_leg': 'mixamorig:RightLeg',
        'right_foot': 'mixamorig:RightFoot',
    }

    def __init__(self):
        self.last_rotations = {}
        self.smoothing = 0.6  # Rotation smoothing factor (higher = smoother but more lag)

    def _get_landmark_3d(self, landmarks, idx, mirror_x=True) -> Optional[np.ndarray]:
        """Get 3D world coordinates for a landmark."""
        if landmarks is None or idx >= len(landmarks):
            return None

        lm = landmarks[idx]
        visibility = getattr(lm, 'visibility', 1.0)
        if visibility is not None and visibility < 0.5:
            return None

        # MediaPipe world coordinates: x=right, y=up, z=towards camera
        x = -lm.x if mirror_x else lm.x
        y = -lm.y  # Flip Y (MediaPipe Y is down)
        z = -lm.z  # Flip Z

        return np.array([x, y, z], dtype=np.float32)

    def _vector_to_rotation(self, from_vec: np.ndarray, to_vec: np.ndarray) -> Quaternion:
        """Calculate rotation quaternion to rotate from_vec to to_vec."""
        from_vec = from_vec / (np.linalg.norm(from_vec) + 1e-8)
        to_vec = to_vec / (np.linalg.norm(to_vec) + 1e-8)

        dot = np.clip(np.dot(from_vec, to_vec), -1.0, 1.0)

        if dot > 0.9999:
            return Quaternion()
        elif dot < -0.9999:
            # 180 degree rotation - find perpendicular axis
            axis = np.cross([1, 0, 0], from_vec)
            if np.linalg.norm(axis) < 0.01:
                axis = np.cross([0, 1, 0], from_vec)
            axis = axis / np.linalg.norm(axis)
            return Quaternion.from_axis_rotation(axis, math.pi)

        axis = np.cross(from_vec, to_vec)
        axis = axis / (np.linalg.norm(axis) + 1e-8)
        angle = math.acos(dot)

        return Quaternion.from_axis_rotation(axis, angle)

    def _smooth_rotation(self, bone_name: str, rotation: Quaternion) -> Quaternion:
        """Apply temporal smoothing to rotation."""
        if bone_name in self.last_rotations:
            last = self.last_rotations[bone_name]
            # Spherical interpolation
            rotation = Quaternion.slerp(last, rotation, 1.0 - self.smoothing)

        self.last_rotations[bone_name] = rotation
        return rotation

    def solve(self, landmarks) -> dict[str, Quaternion]:
        """Convert MediaPipe landmarks to bone rotations.

        Simplified version that only tracks arms to avoid hierarchy cascade issues.
        Arms are most visible in interview-style framing anyway.

        Args:
            landmarks: MediaPipe pose landmarks (world coordinates preferred)

        Returns:
            Dict mapping Mixamo bone names to rotation quaternions
        """
        rotations = {}

        if landmarks is None:
            return rotations

        # Get key landmarks (arms only for now)
        left_shoulder = self._get_landmark_3d(landmarks, self.LEFT_SHOULDER)
        right_shoulder = self._get_landmark_3d(landmarks, self.RIGHT_SHOULDER)
        left_elbow = self._get_landmark_3d(landmarks, self.LEFT_ELBOW)
        right_elbow = self._get_landmark_3d(landmarks, self.RIGHT_ELBOW)
        left_wrist = self._get_landmark_3d(landmarks, self.LEFT_WRIST)
        right_wrist = self._get_landmark_3d(landmarks, self.RIGHT_WRIST)

        # Reference vectors (T-pose orientation)
        # Note: Mixamo T-pose has arms pointing outward along X axis
        right_vec = np.array([1, 0, 0], dtype=np.float32)
        left_vec = np.array([-1, 0, 0], dtype=np.float32)

        # === Arms Only (visible in interview framing) ===
        # We compute rotations as deltas from T-pose direction

        # Left Arm (shoulder to elbow)
        if left_shoulder is not None and left_elbow is not None:
            arm_dir = left_elbow - left_shoulder
            arm_dir = arm_dir / (np.linalg.norm(arm_dir) + 1e-8)
            # Blend with T-pose to reduce extreme rotations
            blended_dir = left_vec * 0.3 + arm_dir * 0.7
            blended_dir = blended_dir / (np.linalg.norm(blended_dir) + 1e-8)
            rot = self._vector_to_rotation(left_vec, blended_dir)
            rot = self._smooth_rotation('left_arm', rot)
            rotations[self.MIXAMO_BONES['left_arm']] = rot

        # Left Forearm (elbow to wrist) - relative to arm
        if left_elbow is not None and left_wrist is not None:
            forearm_dir = left_wrist - left_elbow
            forearm_dir = forearm_dir / (np.linalg.norm(forearm_dir) + 1e-8)
            blended_dir = left_vec * 0.3 + forearm_dir * 0.7
            blended_dir = blended_dir / (np.linalg.norm(blended_dir) + 1e-8)
            rot = self._vector_to_rotation(left_vec, blended_dir)
            rot = self._smooth_rotation('left_forearm', rot)
            rotations[self.MIXAMO_BONES['left_forearm']] = rot

        # Right Arm (shoulder to elbow)
        if right_shoulder is not None and right_elbow is not None:
            arm_dir = right_elbow - right_shoulder
            arm_dir = arm_dir / (np.linalg.norm(arm_dir) + 1e-8)
            blended_dir = right_vec * 0.3 + arm_dir * 0.7
            blended_dir = blended_dir / (np.linalg.norm(blended_dir) + 1e-8)
            rot = self._vector_to_rotation(right_vec, blended_dir)
            rot = self._smooth_rotation('right_arm', rot)
            rotations[self.MIXAMO_BONES['right_arm']] = rot

        # Right Forearm (elbow to wrist)
        if right_elbow is not None and right_wrist is not None:
            forearm_dir = right_wrist - right_elbow
            forearm_dir = forearm_dir / (np.linalg.norm(forearm_dir) + 1e-8)
            blended_dir = right_vec * 0.3 + forearm_dir * 0.7
            blended_dir = blended_dir / (np.linalg.norm(blended_dir) + 1e-8)
            rot = self._vector_to_rotation(right_vec, blended_dir)
            rot = self._smooth_rotation('right_forearm', rot)
            rotations[self.MIXAMO_BONES['right_forearm']] = rot

        # Skip spine/hip/leg rotations - they cascade through hierarchy incorrectly
        # TODO: Implement proper IK or relative rotation system for full body

        return rotations


# =============================================================================
# Bloom Post-Processor
# =============================================================================

class BloomProcessor:
    """Cyberpunk-style bloom post-processing effect."""

    def __init__(self, ctx: moderngl.Context, width: int, height: int):
        self.ctx = ctx
        self.width = width
        self.height = height

        # Bloom parameters
        self.threshold = 0.6  # Brightness threshold for bloom
        self.intensity = 1.2  # Bloom intensity
        self.tint = (0.3, 0.8, 1.0)  # Cyan cyberpunk tint

        # Create shaders
        self.extract_program = ctx.program(
            vertex_shader=FULLSCREEN_VERTEX_SHADER,
            fragment_shader=BLOOM_EXTRACT_SHADER,
        )
        self.blur_program = ctx.program(
            vertex_shader=FULLSCREEN_VERTEX_SHADER,
            fragment_shader=BLUR_SHADER,
        )
        self.composite_program = ctx.program(
            vertex_shader=FULLSCREEN_VERTEX_SHADER,
            fragment_shader=BLOOM_COMPOSITE_SHADER,
        )

        # Fullscreen triangle
        vertices = np.array([-1, -1, 3, -1, -1, 3], dtype=np.float32)
        vbo = ctx.buffer(vertices.tobytes())
        self.vao = ctx.vertex_array(self.extract_program, [(vbo, '2f', 'in_position')])
        self.blur_vao = ctx.vertex_array(self.blur_program, [(vbo, '2f', 'in_position')])
        self.composite_vao = ctx.vertex_array(self.composite_program, [(vbo, '2f', 'in_position')])

        # Create FBOs for bloom passes (half resolution for performance)
        self._create_fbos()

    def _create_fbos(self):
        """Create framebuffers for bloom processing."""
        bloom_w = self.width // 2
        bloom_h = self.height // 2

        self.bloom_tex1 = self.ctx.texture((bloom_w, bloom_h), 4, dtype='f2')
        self.bloom_tex2 = self.ctx.texture((bloom_w, bloom_h), 4, dtype='f2')
        self.bloom_tex1.filter = (moderngl.LINEAR, moderngl.LINEAR)
        self.bloom_tex2.filter = (moderngl.LINEAR, moderngl.LINEAR)

        self.bloom_fbo1 = self.ctx.framebuffer(color_attachments=[self.bloom_tex1])
        self.bloom_fbo2 = self.ctx.framebuffer(color_attachments=[self.bloom_tex2])

    def resize(self, width: int, height: int):
        """Resize bloom buffers."""
        self.width = width
        self.height = height
        self.bloom_tex1.release()
        self.bloom_tex2.release()
        self.bloom_fbo1.release()
        self.bloom_fbo2.release()
        self._create_fbos()

    def apply(self, scene_texture: moderngl.Texture, output_fbo: moderngl.Framebuffer):
        """Apply bloom effect to scene texture and render to output FBO."""
        bloom_w = self.width // 2
        bloom_h = self.height // 2

        # Pass 1: Extract bright pixels
        self.bloom_fbo1.use()
        self.bloom_fbo1.viewport = (0, 0, bloom_w, bloom_h)
        scene_texture.use(0)
        self.extract_program['u_texture'].value = 0
        self.extract_program['u_threshold'].value = self.threshold
        self.vao.render(moderngl.TRIANGLES)

        # Pass 2: Horizontal blur
        self.bloom_fbo2.use()
        self.bloom_tex1.use(0)
        self.blur_program['u_texture'].value = 0
        self.blur_program['u_direction'].value = (1.0 / bloom_w, 0.0)
        self.blur_vao.render(moderngl.TRIANGLES)

        # Pass 3: Vertical blur
        self.bloom_fbo1.use()
        self.bloom_tex2.use(0)
        self.blur_program['u_texture'].value = 0
        self.blur_program['u_direction'].value = (0.0, 1.0 / bloom_h)
        self.blur_vao.render(moderngl.TRIANGLES)

        # Pass 4: Composite
        output_fbo.use()
        output_fbo.viewport = (0, 0, self.width, self.height)
        scene_texture.use(0)
        self.bloom_tex1.use(1)
        self.composite_program['u_scene'].value = 0
        self.composite_program['u_bloom'].value = 1
        self.composite_program['u_bloom_intensity'].value = self.intensity
        self.composite_program['u_tint'].value = self.tint
        self.composite_vao.render(moderngl.TRIANGLES)


# =============================================================================
# Scene3D - Camera, Lighting, and Background
# =============================================================================

class Scene3D:
    """Manages 3D scene with camera, lighting, and optional background."""

    def __init__(self, ctx: moderngl.Context, width: int, height: int):
        self.ctx = ctx
        self.width = width
        self.height = height

        # Camera
        # Interview-style camera - closer, looking at chest level
        self.camera_pos = np.array([0, 0, 2.0], dtype=np.float32)
        self.camera_target = np.array([0, -1.5, 0], dtype=np.float32)
        self.camera_up = np.array([0, 1, 0], dtype=np.float32)
        self.fov = 45.0
        self.near = 0.1
        self.far = 100.0

        # Background
        self.background_texture: Optional[moderngl.Texture] = None
        self.background_program = None
        self.background_vao = None

        self._create_background_renderer()

    def _create_background_renderer(self):
        """Create shader and VAO for background rendering."""
        self.background_program = self.ctx.program(
            vertex_shader=BACKGROUND_VERTEX_SHADER,
            fragment_shader=BACKGROUND_FRAGMENT_SHADER,
        )

        # Fullscreen quad
        vertices = np.array([
            -1, -1, 0, 1,
             1, -1, 1, 1,
            -1,  1, 0, 0,
             1,  1, 1, 0,
        ], dtype=np.float32)

        vbo = self.ctx.buffer(vertices.tobytes())
        self.background_vao = self.ctx.vertex_array(
            self.background_program,
            [(vbo, '2f 2f', 'in_position', 'in_texcoord')],
        )

    def load_background(self, path: Path):
        """Load a background image."""
        from PIL import Image
        img = Image.open(path).convert('RGB')
        img = img.transpose(Image.FLIP_TOP_BOTTOM)

        self.background_texture = self.ctx.texture(img.size, 3, img.tobytes())
        self.background_texture.filter = (moderngl.LINEAR, moderngl.LINEAR)

        logger.info(f"Loaded background: {path} ({img.size[0]}x{img.size[1]})")

    def resize(self, width: int, height: int):
        """Update viewport size."""
        self.width = width
        self.height = height

    def get_view_matrix(self) -> np.ndarray:
        """Get view matrix from camera position."""
        return Matrix44.look_at(
            self.camera_pos,
            self.camera_target,
            self.camera_up,
            dtype=np.float32,
        )

    def get_projection_matrix(self) -> np.ndarray:
        """Get perspective projection matrix."""
        aspect = self.width / max(self.height, 1)
        return Matrix44.perspective_projection(
            self.fov, aspect, self.near, self.far,
            dtype=np.float32,
        )

    def render_background(self):
        """Render background (call before rendering 3D objects)."""
        if self.background_texture is None:
            return

        # Disable depth write for background
        self.ctx.depth_func = '<='

        self.background_texture.use(0)
        self.background_program['u_texture'].value = 0
        self.background_vao.render(moderngl.TRIANGLE_STRIP)

        # Re-enable depth test
        self.ctx.depth_func = '<'


# =============================================================================
# Main Character Renderer Class
# =============================================================================

class CharacterRenderer3D:
    """High-level API for rendering a 3D character driven by pose landmarks."""

    def __init__(self, ctx: moderngl.Context, width: int, height: int):
        self.ctx = ctx
        self.width = width
        self.height = height
        self.scene = Scene3D(ctx, width, height)
        self.mesh_renderer = SkinnedMeshRenderer(ctx)
        self.pose_solver = PoseSolver()
        self.bloom = BloomProcessor(ctx, width, height)

        # Scene FBO for bloom input
        self.scene_texture = ctx.texture((width, height), 4, dtype='f2')
        self.scene_depth = ctx.depth_texture((width, height))
        self.scene_fbo = ctx.framebuffer(
            color_attachments=[self.scene_texture],
            depth_attachment=self.scene_depth,
        )

        # Model transform (position, rotation, scale)
        # Position for interview framing - chest/waist up
        self.model_position = np.array([0, 0, 0], dtype=np.float32)
        # Rotate 180° around X-axis to flip model right-side up (FBX/Mixamo often exports upside down)
        self.model_rotation = np.array([
            [1,  0,  0],
            [0, -1,  0],
            [0,  0, -1],
        ], dtype=np.float32)
        self.model_scale = 1.0

        self._is_loaded = False
        self.enable_bloom = True  # Toggle for bloom effect

    def load_character(self, glb_path: Path):
        """Load character model."""
        self.mesh_renderer.load_model(glb_path)
        self._is_loaded = True
        logger.info(f"Character loaded: {glb_path}")

    def load_background(self, path: Path):
        """Load background image."""
        self.scene.load_background(path)

    def resize(self, width: int, height: int):
        """Update render target size."""
        self.width = width
        self.height = height
        self.scene.resize(width, height)
        self.bloom.resize(width, height)

        # Recreate scene FBO
        self.scene_texture.release()
        self.scene_depth.release()
        self.scene_fbo.release()

        self.scene_texture = self.ctx.texture((width, height), 4, dtype='f2')
        self.scene_depth = self.ctx.depth_texture((width, height))
        self.scene_fbo = self.ctx.framebuffer(
            color_attachments=[self.scene_texture],
            depth_attachment=self.scene_depth,
        )

    def update_pose(self, landmarks):
        """Update character pose from MediaPipe landmarks.

        Args:
            landmarks: MediaPipe pose world landmarks
        """
        if not self._is_loaded:
            return

        rotations = self.pose_solver.solve(landmarks)
        self.mesh_renderer.set_bone_rotations(rotations)

    def get_model_matrix(self) -> np.ndarray:
        """Get model transformation matrix."""
        T = np.eye(4, dtype=np.float32)
        T[:3, 3] = self.model_position

        R = np.eye(4, dtype=np.float32)
        R[:3, :3] = self.model_rotation

        S = np.eye(4, dtype=np.float32) * self.model_scale
        S[3, 3] = 1.0

        return T @ R @ S

    def render(self, output_fbo: Optional[moderngl.Framebuffer] = None):
        """Render the scene (background + character) with bloom post-processing.

        Args:
            output_fbo: Target framebuffer. If None, renders to current context FBO.
        """
        if not self._is_loaded:
            return

        # Render scene to internal FBO for bloom processing
        self.scene_fbo.use()
        self.scene_fbo.viewport = (0, 0, self.width, self.height)
        self.scene_fbo.clear(0.1, 0.1, 0.15, 1.0)

        self.ctx.enable(moderngl.DEPTH_TEST)
        self.ctx.enable(moderngl.CULL_FACE)

        # Get matrices
        view = self.scene.get_view_matrix()
        projection = self.scene.get_projection_matrix()
        model = self.get_model_matrix()

        # Render background first
        self.scene.render_background()

        # Render character
        self.mesh_renderer.render(
            view=view,
            projection=projection,
            model=model,
            camera_pos=self.scene.camera_pos,
        )

        # Apply bloom post-processing
        if output_fbo is not None:
            self.ctx.disable(moderngl.DEPTH_TEST)
            self.ctx.disable(moderngl.CULL_FACE)
            if self.enable_bloom:
                self.bloom.apply(self.scene_texture, output_fbo)
            else:
                # Direct copy without bloom (use bloom's composite with zero intensity)
                self.bloom.intensity = 0.0
                self.bloom.apply(self.scene_texture, output_fbo)
                self.bloom.intensity = 1.2
