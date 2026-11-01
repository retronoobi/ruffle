#version 130
uniform sampler2D image;
uniform sampler2D destination;
uniform int blendMode;
uniform int mode;
uniform int maskWrite;
uniform int spread;
uniform float focal;
uniform vec4 multiply;
uniform vec4 addition;
in vec4 vcolor;
in vec2 vuv;
out vec4 outputColor;
void main(){
 if(mode==5){
   vec4 s=texture(image,vuv),d=texture(destination,vuv);
   if(blendMode==11){outputColor=d*s.a;if(maskWrite!=0&&outputColor.a<=0.0)discard;return;}
   if(blendMode==12){outputColor=d*(1.0-s.a);if(maskWrite!=0&&outputColor.a<=0.0)discard;return;}
   if(blendMode==13){outputColor=s*d.a;if(maskWrite!=0&&outputColor.a<=0.0)discard;return;}
   vec3 a=d.a>0.0?d.rgb/d.a:vec3(0.0),b=s.a>0.0?s.rgb/s.a:vec3(0.0),v=b;
   if(blendMode==1)v=a*b;
   if(blendMode==2)v=a+b-a*b;
   if(blendMode==3)v=max(a,b);
   if(blendMode==4)v=min(a,b);
   if(blendMode==5)v=abs(a-b);
   if(blendMode==6)v=min(vec3(1.0),a+b);
   if(blendMode==7)v=max(vec3(0.0),a-b);
   if(blendMode==8)v=vec3(1.0)-a;
   if(blendMode==9)v=mix(2.0*a*b,1.0-2.0*(1.0-a)*(1.0-b),step(vec3(0.5),a));
   if(blendMode==10)v=mix(2.0*a*b,1.0-2.0*(1.0-a)*(1.0-b),step(vec3(0.5),b));
   outputColor=vec4((1.0-s.a)*d.rgb+(1.0-d.a)*s.rgb+s.a*d.a*v,s.a+d.a-s.a*d.a);
   if(maskWrite!=0&&outputColor.a<=0.0)discard;
   return;
 }
 vec4 c=vcolor;
 if(mode==1){c=texture(image,vuv);if(c.a<=0.0)discard;c.rgb/=c.a;}
 if(mode>=2&&mode<=4){
   float t=vuv.x;
   if(mode==3)t=length(vuv*2.0-1.0);
   if(mode==4){vec2 d=vec2(focal,0.0)-(vuv*2.0-1.0);float len=length(d);d/=max(len,0.000001);t=len/(sqrt(max(0.0,1.0-focal*focal*d.y*d.y))+focal*d.x);}
   if(spread==1)t=fract(t);
   if(spread==2)t=1.0-abs(mod(t,2.0)-1.0);
   c=texture(image,vec2(clamp(t,0.0,1.0),0.5));
 }
 c=clamp(c*multiply+addition,0.0,1.0);
 if(c.a<=0.0)discard;
 outputColor=vec4(c.rgb*c.a,c.a);
}
