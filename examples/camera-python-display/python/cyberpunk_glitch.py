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
- Zero-copy GPU texture binding (stable GL texture IDs)
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

import random


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
        self.gl_ctx = ctx.gpu.gl_context()

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
            'isDramatic': glGetUniformLocation(self.glitch_program, 'isDramatic'),
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

        # Create reusable texture bindings - these have STABLE texture IDs
        # Only needed when glitch is active (not created in true passthrough mode)
        self.input_binding = self.gl_ctx.create_texture_binding()
        self.output_binding = self.gl_ctx.create_texture_binding()

        # Lazy init for output buffer
        self._gpu_ctx = None  # Set in first process() call
        self.output_buffer = None
        self._current_width = 0
        self._current_height = 0
        self._fbo_validated = False  # Only validate FBO on resize, not every frame

        self.gl_initialized = True
        logger.info("Cyberpunk Glitch: OpenGL resources initialized")

    def process(self, ctx):
        """Apply glitch effect using OpenGL shaders."""
        frame = ctx.input("video_in").get()
        if frame is None:
            return

        # Initialize GL resources on first frame (deferred from setup)
        self._init_gl_resources()

        # Get pixel buffer from frame (subprocess returns PixelBuffer directly)
        input_buffer = frame
        width = input_buffer.width
        height = input_buffer.height
        elapsed = ctx.time.elapsed_secs

        # Update glitch state
        glitch_active = self.glitch_state.update(elapsed)

        # TRUE PASSTHROUGH: When not glitching, just forward the input frame directly
        # No GPU work, no texture copies - zero overhead
        if not glitch_active:
            ctx.output("video_out").set(frame)
            self.frame_count += 1
            return

        # Make GL context current
        self.gl_ctx.make_current()

        # Store GPU context reference for lazy buffer allocation
        if self._gpu_ctx is None:
            self._gpu_ctx = ctx.gpu

        # Update input binding (fast rebind, no new GL texture)
        self.input_binding.update(input_buffer)
        input_gl_id = self.input_binding.id
        gl_target = self.input_binding.target  # GL_TEXTURE_RECTANGLE for IOSurface

        # Ensure output buffer exists (lazy init on first use or resize)
        if self.output_buffer is None or self._current_width != width or self._current_height != height:
            # Convert PixelFormat enum to string (acquire_pixel_buffer expects string)
            format_str = str(input_buffer.format).split('.')[-1].lower()
            self.output_buffer = self._gpu_ctx.acquire_pixel_buffer(width, height, format_str)
            self._current_width = width
            self._current_height = height
            self._fbo_validated = False  # Re-validate FBO on resize

        # Update output binding (fast rebind, no new GL texture)
        self.output_binding.update(self.output_buffer)
        output_gl_id = self.output_binding.id

        # Bind FBO with output texture as color attachment
        glBindFramebuffer(GL_FRAMEBUFFER, self.fbo)
        glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, gl_target, output_gl_id, 0)

        # Check FBO status only on first use or after resize (expensive GPU query)
        if not self._fbo_validated:
            status = glCheckFramebufferStatus(GL_FRAMEBUFFER)
            if status != GL_FRAMEBUFFER_COMPLETE:
                logger.warning(f"Framebuffer incomplete: {status}")
                ctx.output("video_out").set(frame)
                return
            self._fbo_validated = True

        # Set viewport
        glViewport(0, 0, width, height)

        # Apply glitch shader (we only reach here when glitch is active)
        glUseProgram(self.glitch_program)
        glUniform1i(self.glitch_uniforms['inputTexture'], 0)
        glUniform2f(self.glitch_uniforms['resolution'], float(width), float(height))
        glUniform1f(self.glitch_uniforms['time'], elapsed)
        glUniform1f(self.glitch_uniforms['intensity'], self.glitch_state.intensity)
        glUniform1f(self.glitch_uniforms['seed'], self.glitch_state.seed)
        glUniform1f(self.glitch_uniforms['isDramatic'], 1.0 if self.glitch_state.is_dramatic else 0.0)

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

        # Output with pixel buffer
        # In subprocess mode, set the PixelBuffer directly (timestamps managed by runtime)
        ctx.output("video_out").set(self.output_buffer)

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
