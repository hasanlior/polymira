//! Names of common PS4 system functions, so an analysis can show readable names instead of
//! NIDs. NIDs are computed from these names (see nid.rs); nothing here comes from Sony's SDK.

use std::collections::HashMap;
use std::sync::OnceLock;

const NAMES: &[&str] = &[
    // libkernel
    "sceKernelDebugOutText", "sceKernelUsleep", "sceKernelGetProcessTime", "sceKernelGetCpumode", "sceKernelIsNeoMode",
    "sceKernelAllocateDirectMemory", "sceKernelMapDirectMemory", "sceKernelReleaseDirectMemory", "sceKernelGetDirectMemorySize",
    "sceKernelMapNamedFlexibleMemory", "sceKernelMunmap", "sceKernelMprotect", "sceKernelOpen", "sceKernelClose", "sceKernelRead",
    "sceKernelWrite", "sceKernelLseek", "sceKernelStat", "sceKernelFstat", "sceKernelMkdir", "sceKernelGetdents",
    "sceKernelCreateEqueue", "sceKernelDeleteEqueue", "sceKernelWaitEqueue", "sceKernelCreateEventFlag", "sceKernelWaitEventFlag",
    "sceKernelSetEventFlag", "sceKernelCreateSema", "sceKernelWaitSema", "sceKernelSignalSema", "sceKernelGettimeofday",
    "sceKernelClockGettime", "sceKernelGetTscFrequency", "sceKernelReadTsc", "sceKernelLoadStartModule", "sceKernelDlsym",
    "scePthreadCreate", "scePthreadJoin", "scePthreadExit", "scePthreadSelf", "scePthreadSetaffinity", "scePthreadSetprio",
    "scePthreadMutexInit", "scePthreadMutexLock", "scePthreadMutexUnlock", "scePthreadMutexDestroy", "scePthreadCondInit",
    "scePthreadCondWait", "scePthreadCondSignal", "scePthreadCondBroadcast", "scePthreadAttrInit", "scePthreadAttrSetstacksize",
    "pthread_create", "pthread_mutex_lock", "pthread_mutex_unlock", "__error", "__stack_chk_fail", "__stack_chk_guard",
    // libc
    "printf", "puts", "sprintf", "snprintf", "vsnprintf", "fprintf", "fopen", "fclose", "fread", "fwrite", "fseek", "ftell",
    "malloc", "free", "calloc", "realloc", "memalign", "memcpy", "memmove", "memset", "memcmp", "strlen", "strcpy", "strncpy",
    "strcmp", "strncmp", "strcat", "strchr", "strstr", "atoi", "strtol", "qsort", "rand", "srand", "time", "exit", "abort",
    "sin", "cos", "tan", "sqrt", "pow", "atan2", "floor", "ceil", "fmod", "sinf", "cosf", "sqrtf", "powf", "atan2f",
    // system services
    "sceUserServiceInitialize", "sceUserServiceGetInitialUser", "sceUserServiceGetUserName", "sceSysmoduleLoadModule",
    "sceSysmoduleUnloadModule", "sceSystemServiceHideSplashScreen", "sceSystemServiceGetStatus", "sceSystemServiceParamGetInt",
    "sceSaveDataInitialize3", "sceSaveDataMount2", "sceSaveDataUmount", "sceNpTrophyCreateContext", "sceNpTrophyUnlockTrophy",
    // video, graphics, audio, input
    "sceVideoOutOpen", "sceVideoOutClose", "sceVideoOutRegisterBuffers", "sceVideoOutSetBufferAttribute", "sceVideoOutSubmitFlip",
    "sceVideoOutSetFlipRate", "sceVideoOutAddFlipEvent", "sceVideoOutGetFlipStatus", "sceVideoOutGetResolutionStatus",
    "sceGnmSubmitCommandBuffers", "sceGnmSubmitAndFlipCommandBuffers", "sceGnmSubmitDone", "sceGnmDrawIndex", "sceGnmDrawIndexAuto",
    "sceGnmSetVsShader", "sceGnmSetPsShader", "sceGnmSetCsShader", "sceGnmDispatchDirect", "sceGnmFlushGarlic",
    "sceAudioOutInit", "sceAudioOutOpen", "sceAudioOutOutput", "sceAudioOutClose", "sceAudioOutSetVolume",
    "scePadInit", "scePadOpen", "scePadClose", "scePadReadState", "scePadRead", "scePadSetVibration", "scePadSetLightBar",
    "sceImeDialogInit", "sceMsgDialogInitialize", "sceCommonDialogInitialize", "sceNetInit", "sceNetCtlInit", "sceHttpInit",
];

/// NID → function name, for the names above.
pub fn names() -> &'static HashMap<String, &'static str> {
    static T: OnceLock<HashMap<String, &'static str>> = OnceLock::new();
    T.get_or_init(|| NAMES.iter().map(|&n| (crate::nid::nid(n), n)).collect())
}
