#version 450

layout(location = 0) in vec3 Vertex_Position;
layout(location = 1) in vec3 Vertex_Normal;
layout(location = 2) in vec2 Vertex_Uv;

layout(location = 0) out vec2 v_Uv;

layout(set = 0, binding = 0) uniform Camera {
    mat4 ViewProj;
};

layout(set = 2, binding = 0) uniform Transform {
    mat4 Model;
};
layout(set = 2, binding = 1) uniform Sprite_size {
    vec2 size;
};
layout(set = 3, binding = 0) uniform UvRect_min {
    vec2 min_uv;
};
layout(set = 3, binding = 1) uniform UvRect_max {
    vec2 max_uv;
};

void main() {
    v_Uv = mix(min_uv, max_uv, Vertex_Uv);
    vec3 position = Vertex_Position * vec3(size, 1.0);
    gl_Position = ViewProj * Model * vec4(position, 1.0);
}
