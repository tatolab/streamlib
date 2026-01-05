# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Cyberpunk glitch post-processing effect using pure OpenGL shaders.

Full-screen glitch effect inspired by Cyberpunk 2077's "Relic Malfunction" visuals.
Uses GLSL fragment shaders directly on GL textures for GPU-accelerated distortion.

Features:
- RGB chromatic aberration (samples input at offset UVs per channel)
- CRT curvature distortion
- Scanlines
- Horizontal slice displacement
- Intermittent triggering (not constant)
- Zero-copy GPU texture sharing via IOSurface
"""

import logging
import math

from OpenGL.GL import *

from streamlib import processor, input, output

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

// Hash function for pseudo-random values
float hash11(float p) {
    p = fract(p * 0.1031);
    p *= p + 33.33;
    p *= p + p;
    return fract(p);
}

void main() {
    vec2 uv = texCoord / resolution;  // Normalized 0-1
    vec2 sampleCoord = texCoord;  // Use original coordinates (no curvature)

    // No effect when intensity is 0
    if (intensity < 0.01) {
        fragColor = texture(inputTexture, texCoord);
        return;
    }

    // === RGB Chromatic Aberration ===
    float aberration = 8.0 * intensity;
    vec2 rOffset = vec2(aberration * (hash11(seed * 1.1) - 0.5) * 2.0, 0.0);
    vec2 bOffset = vec2(aberration * (hash11(seed * 2.2) - 0.5) * 2.0, 0.0);

    float r = texture(inputTexture, sampleCoord + rOffset).r;
    float g = texture(inputTexture, sampleCoord).g;
    float b = texture(inputTexture, sampleCoord + bOffset).b;

    vec3 color = vec3(r, g, b);

    // === Scanlines ===
    float scanline = sin(texCoord.y * 3.14159 * 2.0) * 0.5 + 0.5;
    scanline = pow(scanline, 0.5);
    color *= 0.85 + 0.15 * scanline;

    // === Horizontal slice displacement ===
    // More slices (60 rows) with lower threshold (0.75) for more visible effect
    float numSlices = 60.0;
    float sliceNoise = hash11(floor(uv.y * numSlices) + seed);
    if (sliceNoise > 0.75 && intensity > 0.3) {
        // Vary slice offset based on position and seed for variety
        float sliceStrength = (sliceNoise - 0.75) * 4.0;  // 0-1 range for slices that pass
        float sliceOffset = (hash11(seed + floor(uv.y * numSlices) * 0.1) - 0.5) * 60.0 * intensity * sliceStrength;
        vec2 sliceCoord = sampleCoord + vec2(sliceOffset, 0.0);
        color = texture(inputTexture, sliceCoord).rgb;
    }

    // === Additional fine slice displacement (smaller, more frequent) ===
    float fineSliceNoise = hash11(floor(uv.y * 120.0) + seed * 1.7);
    if (fineSliceNoise > 0.85 && intensity > 0.4) {
        float fineOffset = (hash11(seed * 2.3 + floor(uv.y * 120.0) * 0.1) - 0.5) * 25.0 * intensity;
        vec2 fineSliceCoord = sampleCoord + vec2(fineOffset, 0.0);
        color = mix(color, texture(inputTexture, fineSliceCoord).rgb, 0.7);
    }

    // === Random noise lines ===
    float lineNoise = hash11(time * 50.0 + floor(uv.y * resolution.y));
    if (lineNoise > 0.97) {
        color += vec3(0.0, 0.3 * intensity, 0.3 * intensity);
    }

    fragColor = vec4(color, 1.0);
}
"""

# Simple passthrough with subtle scanlines
PASSTHROUGH_FRAGMENT_SHADER = """
#version 150 core

in vec2 texCoord;
out vec4 fragColor;

uniform sampler2DRect inputTexture;

void main() {
    vec4 color = texture(inputTexture, texCoord);
    // Subtle scanlines
    float scanline = sin(texCoord.y * 3.14159) * 0.5 + 0.5;
    color.rgb *= 0.95 + 0.05 * scanline;
    fragColor = color;
}
"""


# =============================================================================
# Glitch State Tracking
# =============================================================================

def hash11(p: float) -> float:
    """Simple hash function for pseudo-random values."""
    p = (p * 0.1031) % 1.0
    p *= p + 33.33
    p *= p + p
    return p % 1.0


class GlitchState:
    """Tracks glitch effect state and timing."""

    def __init__(self):
        self.active = False
        self.intensity = 0.0
        self.start_time = 0.0
        self.duration = 0.0
        self.last_check = 0.0
        self.seed = 0.0

    def update(self, elapsed: float) -> bool:
        """Update glitch state, returns True if glitch should be active."""
        if elapsed - self.last_check > 0.1:
            self.last_check = elapsed

            if not self.active:
                if hash11(elapsed * 7.3) > 0.85:
                    self.active = True
                    self.start_time = elapsed
                    self.duration = 0.1 + hash11(elapsed * 3.7) * 0.4
                    self.intensity = 0.3 + hash11(elapsed * 11.1) * 0.7
                    self.seed = elapsed
            else:
                if elapsed - self.start_time > self.duration:
                    self.active = False
                    self.intensity = 0.0

        return self.active


# =============================================================================
# Cyberpunk Glitch Processor
# =============================================================================

@processor(name="CyberpunkGlitch", description="Cyberpunk glitch post-processing effect")
class CyberpunkGlitch:
    """Full-screen glitch effect using pure OpenGL shaders.

    Bypasses Skia and runs GLSL fragment shaders directly on the GL textures
    for maximum performance and flexibility.
    """

    @input(schema="VideoFrame")
    def video_in(self):
        pass

    @output(schema="VideoFrame")
    def video_out(self):
        pass

    def setup(self, ctx):
        """Initialize processor state. GL resources created lazily in first process() call."""
        self.frame_count = 0
        self.glitch_state = GlitchState()
        self.gl_initialized = False

        # Get GL context reference (but don't initialize GL resources yet)
        self.gl_ctx = ctx.gpu._experimental_gl_context()

        logger.info("Cyberpunk Glitch processor setup complete (GL init deferred)")

    def _init_gl_resources(self):
        """Initialize OpenGL resources. Called on first process() when GL state is ready."""
        if self.gl_initialized:
            return

        self.gl_ctx.make_current()

        # Helper to compile a shader using raw GL calls (avoids PyOpenGL validation)
        def compile_shader(source: str, shader_type) -> int:
            shader = glCreateShader(shader_type)
            glShaderSource(shader, source)
            glCompileShader(shader)
            if not glGetShaderiv(shader, GL_COMPILE_STATUS):
                error = glGetShaderInfoLog(shader).decode('utf-8')
                glDeleteShader(shader)
                raise RuntimeError(f"Shader compilation failed: {error}")
            return shader

        # Helper to link a program
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

        # Compile shaders
        try:
            vertex_shader = compile_shader(VERTEX_SHADER, GL_VERTEX_SHADER)
            glitch_frag = compile_shader(GLITCH_FRAGMENT_SHADER, GL_FRAGMENT_SHADER)
            passthrough_frag = compile_shader(PASSTHROUGH_FRAGMENT_SHADER, GL_FRAGMENT_SHADER)

            self.glitch_program = link_program(vertex_shader, glitch_frag)
            self.passthrough_program = link_program(vertex_shader, passthrough_frag)

            # Clean up shader objects (they're linked into programs now)
            glDeleteShader(vertex_shader)
            glDeleteShader(glitch_frag)
            glDeleteShader(passthrough_frag)
        except Exception as e:
            logger.error(f"Shader compilation failed: {e}")
            raise RuntimeError(f"Failed to compile shaders: {e}")

        # Get uniform locations for glitch shader
        glUseProgram(self.glitch_program)
        self.glitch_uniforms = {
            'inputTexture': glGetUniformLocation(self.glitch_program, 'inputTexture'),
            'resolution': glGetUniformLocation(self.glitch_program, 'resolution'),
            'time': glGetUniformLocation(self.glitch_program, 'time'),
            'intensity': glGetUniformLocation(self.glitch_program, 'intensity'),
            'seed': glGetUniformLocation(self.glitch_program, 'seed'),
        }

        # Get uniform locations for passthrough shader
        glUseProgram(self.passthrough_program)
        self.passthrough_uniforms = {
            'inputTexture': glGetUniformLocation(self.passthrough_program, 'inputTexture'),
            'resolution': glGetUniformLocation(self.passthrough_program, 'resolution'),
        }

        # Create fullscreen quad VAO
        import numpy as np

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
        glUseProgram(0)

        # Create FBO for rendering to output texture
        self.fbo = glGenFramebuffers(1)

        self.gl_initialized = True
        logger.info("Cyberpunk Glitch: OpenGL resources initialized")

    def process(self, ctx):
        """Apply glitch effect using OpenGL shaders."""
        frame = ctx.input("video_in").get()
        if frame is None:
            return

        # Initialize GL resources on first frame (deferred from setup)
        self._init_gl_resources()

        width = frame["width"]
        height = frame["height"]
        input_texture = frame["texture"]
        elapsed = ctx.time.elapsed_secs

        # Update glitch state
        glitch_active = self.glitch_state.update(elapsed)

        # Make GL context current
        self.gl_ctx.make_current()

        # Acquire output surface
        output_tex = ctx.gpu.acquire_surface(width, height)

        # Get GL texture IDs
        input_gl_id = input_texture._experimental_gl_texture_id(self.gl_ctx)
        output_gl_id = output_tex._experimental_gl_texture_id(self.gl_ctx)
        gl_target = self.gl_ctx.texture_target  # GL_TEXTURE_RECTANGLE for IOSurface

        # Bind FBO with output texture as color attachment
        glBindFramebuffer(GL_FRAMEBUFFER, self.fbo)
        glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, gl_target, output_gl_id, 0)

        # Check FBO status
        status = glCheckFramebufferStatus(GL_FRAMEBUFFER)
        if status != GL_FRAMEBUFFER_COMPLETE:
            logger.warning(f"Framebuffer incomplete: {status}")
            ctx.output("video_out").set(frame)
            return

        # Set viewport
        glViewport(0, 0, width, height)

        # Choose shader based on glitch state
        if glitch_active:
            glUseProgram(self.glitch_program)
            glUniform1i(self.glitch_uniforms['inputTexture'], 0)
            glUniform2f(self.glitch_uniforms['resolution'], float(width), float(height))
            glUniform1f(self.glitch_uniforms['time'], elapsed)
            glUniform1f(self.glitch_uniforms['intensity'], self.glitch_state.intensity)
            glUniform1f(self.glitch_uniforms['seed'], self.glitch_state.seed)
        else:
            glUseProgram(self.passthrough_program)
            glUniform1i(self.passthrough_uniforms['inputTexture'], 0)
            glUniform2f(self.passthrough_uniforms['resolution'], float(width), float(height))

        # Bind input texture
        glActiveTexture(GL_TEXTURE0)
        glBindTexture(gl_target, input_gl_id)

        # Draw fullscreen quad
        glBindVertexArray(self.vao)
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4)
        glBindVertexArray(0)

        # Cleanup
        glBindTexture(gl_target, 0)
        glBindFramebuffer(GL_FRAMEBUFFER, 0)
        glUseProgram(0)

        # Flush GL commands
        self.gl_ctx.flush()

        # Output
        ctx.output("video_out").set({
            "texture": output_tex.texture,
            "width": width,
            "height": height,
            "timestamp_ns": frame["timestamp_ns"],
            "frame_number": frame["frame_number"],
        })

        self.frame_count += 1
        if self.frame_count % 120 == 0:
            logger.debug(f"Glitch processor: {self.frame_count} frames (active={glitch_active})")

    def teardown(self, ctx):
        """Cleanup OpenGL resources."""
        self.gl_ctx.make_current()

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

        logger.info(f"Cyberpunk Glitch processor shutdown ({self.frame_count} frames)")
