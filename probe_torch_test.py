import sys, json, platform
try:
    import torch
    torch_info = {
        "installed": True,
        "version": torch.__version__,
        "cuda_available": torch.cuda.is_available(),
        "cuda_version": str(torch.version.cuda) if torch.version.cuda else None,
    }
except Exception as e:
    torch_info = {"installed": False, "error": str(e)}
print(json.dumps({"torch": torch_info, "platform": {"system": platform.system(), "release": platform.release()}, "executable": sys.executable}))
