#version 130
in vec2 position;
in vec4 color;
in vec2 uv;
uniform mat3 world;
uniform vec2 viewport;
out vec4 vcolor;
out vec2 vuv;
void main(){vec3 p=world*vec3(position,1);gl_Position=vec4(p.x/viewport.x*2.0-1.0,1.0-p.y/viewport.y*2.0,0,1);vcolor=color;vuv=uv;}
