# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Cyberpunk glitch post-processing effect using pure OpenGL shaders.

Isolated subprocess processor using standalone CGL context.
Full-screen glitch effect inspired by Cyberpunk 2077's "Relic Malfunction" visuals.
Uses GLSL fragment shaders directly on GL textures for GPU-accelerated distortion.

Features:
- RGB chromatic aberration (samples input at offset UVs per channel)
- CRT curvature distortion
- Scanlines
- Horizontal slice displacement
- Intermittent triggering (not constant)
- Zero-copy GPU texture via IOSurface + CGL binding
"""

import logging
import math
import random

import numpy as np
from OpenGL.GL import *

logger = logging.getLogger(__name__)


# =============================================================================
# GLSL Shaders
# =============================================================================

# Vertex shader - simple fullscreen quad
VERTEX_SHADER = """
#version 150 core

in vec2 position;
out vec2 texCoord;

uniform vec2 resolution;

void main() {
    gl_Position = vec4(position, 0.0, 1.0);
    // Convert from clip space (-1 to 1) to texture space (0 to resolution)
    texCoord = (position * 0.5 + 0.5) * resolution;
}
"""

# Fragment shader for glitch effect
# Note: Uses sampler2DRect for IOSurface textures (pixel coordinates, not normalized)
GLITCH_FRAGMENT_SHADER = """
#version 150 core

in vec2 texCoord;
out vec4 fragColor;

uniform sampler2DRect inputTexture;
uniform vec2 resolution;
uniform float time;
uniform float intensity;
uniform float seed;
uniform float isDramatic;  // 1.0 for dramatic mode, 0.0 for normal

// Hash function for pseudo-random values
float hash11(float p) {
    p = fract(p * 0.1031);
    p *= p + 33.33;
    p *= p + p;
    return fract(p);
}

// 2D hash for block noise
float hash21(vec2 p) {
    p = fract(p * vec2(234.34, 435.345));
    p += dot(p, p + 34.23);
    return fract(p.x * p.y);
}

void main() {
    vec2 uv = texCoord / resolution;  // Normalized 0-1
    vec2 sampleCoord = texCoord;

    // No effect when intensity is 0
    if (intensity < 0.01) {
        fragColor = texture(inputTexture, texCoord);
        return;
    }

    vec3 color;

    // =========================================================================
    // DRAMATIC MODE - Horizontal slice displacement + film grain
    // =========================================================================
    if (isDramatic > 0.5) {
        // Divide screen into rows of ~15-30px height
        // Use seed to vary slice height between frames
        float sliceHeight = mix(15.0, 30.0, hash11(seed * 0.5));
        float sliceIndex = floor(texCoord.y / sliceHeight);

        // Random horizontal offset for this slice (0-200 pixels, left or right)
        float sliceRandom = hash11(sliceIndex + seed);
        float maxOffset = 200.0 * intensity;
        float xOffset = (sliceRandom - 0.5) * 2.0 * maxOffset;

        // Sample with horizontal displacement
        vec2 displacedCoord = sampleCoord + vec2(xOffset, 0.0);
        color = texture(inputTexture, displacedCoord).rgb;

        // === FILM GRAIN (Perlin-like noise) ===
        // Multi-octave noise for organic film grain look
        float grain = 0.0;
        float grainScale = 1.0;
        float grainAmp = 0.5;
        for (int i = 0; i < 3; i++) {
            vec2 grainUV = uv * resolution * grainScale * 0.01 + seed;
            grain += (hash21(grainUV) - 0.5) * grainAmp;
            grainScale *= 2.0;
            grainAmp *= 0.5;
        }

        // Add temporal variation to grain
        grain += (hash21(uv * 800.0 + time * 10.0) - 0.5) * 0.15;

        // Apply grain (subtle, film-like)
        color += grain * 0.12 * intensity;
        color = clamp(color, 0.0, 1.0);
    }
    // =========================================================================
    // NORMAL MODE - Subtle glitch
    // =========================================================================
    else {
        // === RGB Chromatic Aberration ===
        float aberration = 8.0 * intensity;
        vec2 rOffset = vec2(aberration * (hash11(seed * 1.1) - 0.5) * 2.0, 0.0);
        vec2 bOffset = vec2(aberration * (hash11(seed * 2.2) - 0.5) * 2.0, 0.0);

        float r = texture(inputTexture, sampleCoord + rOffset).r;
        float g = texture(inputTexture, sampleCoord).g;
        float b = texture(inputTexture, sampleCoord + bOffset).b;

        color = vec3(r, g, b);

        // === Scanlines ===
        float scanline = sin(texCoord.y * 3.14159 * 2.0) * 0.5 + 0.5;
        scanline = pow(scanline, 0.5);
        color *= 0.85 + 0.15 * scanline;

        // === Horizontal slice displacement (sparse) ===
        float sliceNoise = hash11(floor(uv.y * 60.0) + seed);
        if (sliceNoise > 0.75 && intensity > 0.3) {
            float sliceStrength = (sliceNoise - 0.75) / 0.25;
            float sliceOffset = (hash11(seed + floor(uv.y * 60.0) * 0.1) - 0.5) * 60.0 * intensity * sliceStrength;
            vec2 sliceCoord = sampleCoord + vec2(sliceOffset, 0.0);
            color = texture(inputTexture, sliceCoord).rgb;
        }

        // === Random noise lines ===
        float lineNoise = hash11(time * 50.0 + floor(uv.y * resolution.y));
        if (lineNoise > 0.97) {
            color += vec3(0.0, 0.3 * intensity, 0.3 * intensity);
        }
    }

    fragColor = vec4(color, 1.0);
}
"""

# True passthrough - no effects when not glitching
PASSTHROUGH_FRAGMENT_SHADER = """
#version 150 core

in vec2 texCoord;
out vec4 fragColor;

uniform sampler2DRect inputTexture;

void main() {
    fragColor = texture(inputTexture, texCoord);
}
"""


# =============================================================================
# Glitch State Tracking
# =============================================================================

class GlitchState:
    """Tracks glitch effect state and timing.

    Single timer triggers every 0-8 seconds (after 2s cooldown).
    Randomly chooses between minor or major glitch each time.
    """

    COOLDOWN = 2.0  # 2 second delay after glitch ends before scheduling next

    def __init__(self):
        self.active = False
        self.is_dramatic = False
        self.intensity = 0.0
        self.start_time = 0.0
        self.duration = 0.0
        self.seed = 0.0
        self.in_cooldown = False
        self.cooldown_end_time = 0.0

        # Single timer for next glitch (0-8 seconds)
        self.next_glitch = random.uniform(0.0, 8.0)

    def update(self, elapsed: float) -> bool:
        """Update glitch state, returns True if glitch should be active."""

        # If currently glitching, check if it should end
        if self.active:
            if elapsed - self.start_time > self.duration:
                self.active = False
                self.is_dramatic = False
                self.intensity = 0.0
                # Start cooldown period
                self.in_cooldown = True
                self.cooldown_end_time = elapsed + self.COOLDOWN
            return self.active

        # If in cooldown, check if it's over and schedule next glitch
        if self.in_cooldown:
            if elapsed >= self.cooldown_end_time:
                self.in_cooldown = False
                # Schedule next glitch (random 0-8 seconds from now)
                self.next_glitch = elapsed + random.uniform(0.0, 8.0)
            return False

        # Check if it's time for a glitch
        if elapsed >= self.next_glitch:
            self.active = True
            self.start_time = elapsed
            self.seed = elapsed

            # Randomly choose major or minor
            if random.random() < 0.5:
                # Major (dramatic) glitch
                self.is_dramatic = True
                self.duration = random.uniform(0.3, 0.8)
                self.intensity = random.uniform(0.8, 1.0)
            else:
                # Minor (normal) glitch
                self.is_dramatic = False
                self.duration = random.uniform(0.1, 0.3)
                self.intensity = random.uniform(0.3, 0.6)

            return True

        return False


# =============================================================================
# Cyberpunk Glitch Processor (Isolated Subprocess)
# =============================================================================

class CyberpunkGlitch:
    """Full-screen glitch effect using pure OpenGL shaders.

    Isolated subprocess processor with own CGL context.
    Runs GLSL fragment shaders directly on GL textures backed by IOSurfaces.
    """

    def setup(self, ctx):
        """Initialize standalone CGL context and compile shaders."""
        from streamlib.cgl_context import create_cgl_context, make_current

        self.frame_count = 0
        self.glitch_state = GlitchState()

        # Create standalone CGL context (own GPU context, not host's)
        self.cgl_ctx = create_cgl_context()
        make_current(self.cgl_ctx)

        # Compile shaders
        self._compile_shaders()

        # Create fullscreen quad VAO
        self._create_quad_vao()

        # Create FBO for rendering to output texture
        self.fbo = glGenFramebuffers(1)

        # Create reusable GL textures for input and output IOSurface binding
        self.input_tex_id = glGenTextures(1)
        self.output_tex_id = glGenTextures(1)

        # Track current dimensions for lazy output buffer allocation
        self._current_width = 0
        self._current_height = 0
        self._fbo_validated = False

        logger.info("Cyberpunk Glitch: Standalone CGL context + shaders initialized")

    def _compile_shaders(self):
        """Compile and link GLSL shader programs."""
        def compile_shader(source: str, shader_type) -> int:
            shader = glCreateShader(shader_type)
            glShaderSource(shader, source)
            glCompileShader(shader)
            if not glGetShaderiv(shader, GL_COMPILE_STATUS):
                error = glGetShaderInfoLog(shader).decode('utf-8')
                glDeleteShader(shader)
                raise RuntimeError(f"Shader compilation failed: {error}")
            return shader

        def link_program(vertex_shader, fragment_shader) -> int:
            program = glCreateProgram()
            glAttachShader(program, vertex_shader)
            glAttachShader(program, fragment_shader)
            glLinkProgram(program)
            if not glGetProgramiv(program, GL_LINK_STATUS):
                error = glGetProgramInfoLog(program).decode('utf-8')
                glDeleteProgram(program)
                raise RuntimeError(f"Program linking failed: {error}")
            return program

        vertex_shader = compile_shader(VERTEX_SHADER, GL_VERTEX_SHADER)
        glitch_frag = compile_shader(GLITCH_FRAGMENT_SHADER, GL_FRAGMENT_SHADER)
        passthrough_frag = compile_shader(PASSTHROUGH_FRAGMENT_SHADER, GL_FRAGMENT_SHADER)

        self.glitch_program = link_program(vertex_shader, glitch_frag)
        self.passthrough_program = link_program(vertex_shader, passthrough_frag)

        # Clean up shader objects (they're linked into programs now)
        glDeleteShader(vertex_shader)
        glDeleteShader(glitch_frag)
        glDeleteShader(passthrough_frag)

        # Get uniform locations for glitch shader
        glUseProgram(self.glitch_program)
        self.glitch_uniforms = {
            'inputTexture': glGetUniformLocation(self.glitch_program, 'inputTexture'),
            'resolution': glGetUniformLocation(self.glitch_program, 'resolution'),
            'time': glGetUniformLocation(self.glitch_program, 'time'),
            'intensity': glGetUniformLocation(self.glitch_program, 'intensity'),
            'seed': glGetUniformLocation(self.glitch_program, 'seed'),
            'isDramatic': glGetUniformLocation(self.glitch_program, 'isDramatic'),
        }

        # Get uniform locations for passthrough shader
        glUseProgram(self.passthrough_program)
        self.passthrough_uniforms = {
            'inputTexture': glGetUniformLocation(self.passthrough_program, 'inputTexture'),
            'resolution': glGetUniformLocation(self.passthrough_program, 'resolution'),
        }

        glUseProgram(0)

    def _create_quad_vao(self):
        """Create fullscreen quad VAO."""
        self.vao = glGenVertexArrays(1)
        self.vbo = glGenBuffers(1)

        glBindVertexArray(self.vao)
        glBindBuffer(GL_ARRAY_BUFFER, self.vbo)

        # Fullscreen quad vertices (clip space coordinates)
        vertices = np.array([
            -1.0, -1.0,
             1.0, -1.0,
            -1.0,  1.0,
             1.0,  1.0,
        ], dtype=np.float32)

        glBufferData(GL_ARRAY_BUFFER, vertices.nbytes, vertices, GL_STATIC_DRAW)

        # Position attribute
        position_loc = glGetAttribLocation(self.glitch_program, 'position')
        glEnableVertexAttribArray(position_loc)
        glVertexAttribPointer(position_loc, 2, GL_FLOAT, GL_FALSE, 0, None)

        glBindVertexArray(0)

    def process(self, ctx):
        """Apply glitch effect using OpenGL shaders."""
        frame = ctx.inputs.read("video_in")
        if frame is None:
            return

        from streamlib.cgl_context import make_current, bind_iosurface_to_texture, flush, GL_TEXTURE_RECTANGLE
        import time as time_mod

        # Frame limiter: cap at 60fps to match display vsync.
        # With 3 pool surfaces at 250+fps, surfaces recycle every ~12ms
        # which is faster than the 16.7ms display refresh.
        now = time_mod.monotonic()
        if hasattr(self, '_last_output_time'):
            if now - self._last_output_time < 1.0 / 60.0:
                return

        w = frame["width"]
        h = frame["height"]

        # Use monotonic time for elapsed calculation
        if not hasattr(self, '_start_time'):
            self._start_time = now
        elapsed = now - self._start_time

        # Update glitch state
        glitch_active = self.glitch_state.update(elapsed)

        make_current(self.cgl_ctx)

        # Resolve input surface → IOSurface handle → bind as GL texture
        input_handle = ctx.gpu.resolve_surface(frame["surface_id"])
        bind_iosurface_to_texture(
            self.cgl_ctx, self.input_tex_id,
            input_handle.iosurface_ref, w, h
        )

        # Acquire output surface → bind as GL texture
        out_surface_id, output_handle = ctx.gpu.acquire_surface(width=w, height=h, format="bgra")
        bind_iosurface_to_texture(
            self.cgl_ctx, self.output_tex_id,
            output_handle.iosurface_ref, w, h
        )

        # Validate FBO on dimension change
        if self._current_width != w or self._current_height != h:
            self._current_width = w
            self._current_height = h
            self._fbo_validated = False

        # Bind FBO with output texture as color attachment
        glBindFramebuffer(GL_FRAMEBUFFER, self.fbo)
        glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_RECTANGLE, self.output_tex_id, 0)

        # Check FBO status only on first use or after resize
        if not self._fbo_validated:
            status = glCheckFramebufferStatus(GL_FRAMEBUFFER)
            if status != GL_FRAMEBUFFER_COMPLETE:
                logger.warning(f"Framebuffer incomplete: {status}")
                input_handle.release()
                output_handle.release()
                ctx.outputs.write("video_out", frame)
                return
            self._fbo_validated = True

        # Set viewport
        glViewport(0, 0, w, h)

        # Always use glitch program — intensity=0 triggers early-return passthrough
        # in the shader (line 84). This avoids potential GL attribute location
        # mismatch between the glitch and passthrough programs (VAO was bound
        # to the glitch program's 'position' attribute location).
        glUseProgram(self.glitch_program)
        glUniform1i(self.glitch_uniforms['inputTexture'], 0)
        glUniform2f(self.glitch_uniforms['resolution'], float(w), float(h))
        if glitch_active:
            glUniform1f(self.glitch_uniforms['time'], elapsed)
            glUniform1f(self.glitch_uniforms['intensity'], self.glitch_state.intensity)
            glUniform1f(self.glitch_uniforms['seed'], self.glitch_state.seed)
            glUniform1f(self.glitch_uniforms['isDramatic'], 1.0 if self.glitch_state.is_dramatic else 0.0)
        else:
            glUniform1f(self.glitch_uniforms['intensity'], 0.0)

        # Bind input texture
        glActiveTexture(GL_TEXTURE0)
        glBindTexture(GL_TEXTURE_RECTANGLE, self.input_tex_id)

        # Draw fullscreen quad
        glBindVertexArray(self.vao)
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4)
        glBindVertexArray(0)

        # Cleanup
        glBindTexture(GL_TEXTURE_RECTANGLE, 0)
        glBindFramebuffer(GL_FRAMEBUFFER, 0)
        glUseProgram(0)

        # Flush GL commands
        flush()

        # Release IOSurface references
        input_handle.release()
        output_handle.release()

        # Output frame with new surface_id
        out_frame = dict(frame)  # copy input frame
        out_frame["surface_id"] = out_surface_id
        ctx.outputs.write("video_out", out_frame)
        self._last_output_time = time_mod.monotonic()

        self.frame_count += 1
        if self.frame_count % 120 == 0:
            logger.debug(f"Glitch processor: {self.frame_count} frames (active={glitch_active})")

    def teardown(self, ctx):
        """Cleanup OpenGL resources."""
        from streamlib.cgl_context import make_current, destroy_cgl_context

        if hasattr(self, 'cgl_ctx'):
            make_current(self.cgl_ctx)

            if hasattr(self, 'vao'):
                glDeleteVertexArrays(1, [self.vao])
            if hasattr(self, 'vbo'):
                glDeleteBuffers(1, [self.vbo])
            if hasattr(self, 'fbo'):
                glDeleteFramebuffers(1, [self.fbo])
            if hasattr(self, 'glitch_program'):
                glDeleteProgram(self.glitch_program)
            if hasattr(self, 'passthrough_program'):
                glDeleteProgram(self.passthrough_program)

            destroy_cgl_context(self.cgl_ctx)

        logger.info(f"Cyberpunk Glitch processor shutdown ({self.frame_count} frames)")
