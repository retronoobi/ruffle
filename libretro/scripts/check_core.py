"""Load the produced Windows core and verify its public libretro ABI, without GL."""
import argparse
import ctypes as c
from pathlib import Path

parser=argparse.ArgumentParser()
parser.add_argument('--dll',type=Path,required=True)
parser.add_argument('--info',type=Path,required=True)
parser.add_argument('--version',required=True)
args=parser.parse_args()

class SystemInfo(c.Structure):
    _fields_=[('name',c.c_char_p),('version',c.c_char_p),('extensions',c.c_char_p),('fullpath',c.c_bool),('block_extract',c.c_bool)]

core=c.CDLL(str(args.dll.resolve()))
exports=['retro_api_version','retro_init','retro_deinit','retro_get_system_info','retro_get_system_av_info','retro_set_environment','retro_set_video_refresh','retro_set_audio_sample','retro_set_audio_sample_batch','retro_set_input_poll','retro_set_input_state','retro_set_controller_port_device','retro_reset','retro_run','retro_load_game','retro_load_game_special','retro_unload_game','retro_get_region','retro_serialize_size','retro_serialize','retro_unserialize','retro_cheat_reset','retro_cheat_set','retro_get_memory_data','retro_get_memory_size']
for name in exports:
    if not hasattr(core,name):
        raise SystemExit(f'Missing required libretro export: {name}')
env_type=c.CFUNCTYPE(c.c_bool,c.c_uint,c.c_void_p)
environment=env_type(lambda command,data:False)
core.retro_set_environment.argtypes=[env_type]
core.retro_set_environment.restype=None
core.retro_set_environment(environment)
core.retro_api_version.restype=c.c_uint
assert core.retro_api_version()==1
core.retro_get_system_info.argtypes=[c.POINTER(SystemInfo)]
core.retro_get_system_info.restype=None
info=SystemInfo()
core.retro_get_system_info(c.byref(info))
assert info.name==b'Ruffle'
assert info.version.decode()==args.version+' OpenGL'
assert info.extensions==b'swf' and info.fullpath
core.retro_serialize_size.restype=c.c_size_t
assert core.retro_serialize_size()==16*1024*1024
for name in ['retro_serialize','retro_unserialize']:
    function=getattr(core,name)
    function.argtypes=[c.c_void_p,c.c_size_t]
    function.restype=c.c_bool
    assert not function(None,0)
core.retro_load_game.argtypes=[c.c_void_p]
core.retro_load_game.restype=c.c_bool
assert not core.retro_load_game(None)
core.retro_init.restype=None
core.retro_deinit.restype=None
core.retro_init()
core.retro_deinit()
metadata=args.info.read_text(encoding='utf-8')
assert f'display_version = "{args.version}"' in metadata
assert 'supported_extensions = "swf"' in metadata
assert 'Network access is permanently disabled' in metadata
print(f'Validated libretro API 1, required exports, metadata and version {args.version}.')
