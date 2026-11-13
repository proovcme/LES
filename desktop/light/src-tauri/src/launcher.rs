//! The desktop starts the native owner using the same contract as the installer.
use std::{path::Path, process::Command};

#[cfg(windows)]
pub fn pick_folder(owner: isize, initial: Option<String>) -> Result<Option<String>, String> {
    use std::sync::Mutex;
    use windows::{
        core::{w, HRESULT, PCWSTR},
        Win32::{
            Foundation::HWND,
            System::Com::{
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
            },
            UI::Shell::{
                FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName,
                FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS,
                SIGDN_FILESYSPATH,
            },
        },
    };
    static PICKER: Mutex<()> = Mutex::new(());
    let _picker = PICKER.try_lock().map_err(|_| "Выбор папки уже открыт.")?;
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }

    let select = || unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let _apartment = Apartment;
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(
            dialog.GetOptions()?
                | FOS_PICKFOLDERS
                | FOS_FORCEFILESYSTEM
                | FOS_PATHMUSTEXIST
                | FOS_NOCHANGEDIR,
        )?;
        dialog.SetTitle(w!("Выберите папку с документами"))?;
        dialog.SetOkButtonLabel(w!("Выбрать папку"))?;
        if let Some(path) =
            initial.filter(|value| Path::new(value).is_absolute() && !value.contains('\0'))
        {
            let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
            if let Ok(folder) =
                SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(path.as_ptr()), None)
            {
                dialog.SetFolder(&folder)?;
            }
        }
        match dialog.Show(Some(HWND(owner as *mut _))) {
            Err(error) if error.code() == HRESULT(0x800704c7u32 as i32) => return Ok(None),
            result => result?,
        }
        let selected = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = selected.to_string();
        CoTaskMemFree(Some(selected.0.cast()));
        path.map(Some).map_err(|_| {
            windows::core::Error::new(HRESULT(0x80070057u32 as i32), "Некорректный путь")
        })
    };
    select().map_err(|_: windows::core::Error| "Не удалось открыть выбор папки.".into())
}

pub fn command(directory: &Path, root: &Path, state: &Path) -> Result<Command, &'static str> {
    let executable = directory.join("native/runtime/les-runtime.exe");
    if !executable.is_file() {
        return Err("В установке отсутствует служба запуска Леса. Восстановите приложение.");
    }
    let mut command = Command::new(executable);
    command
        .arg("--native-only")
        .arg("--root")
        .arg(root)
        .arg("--state")
        .arg(state)
        .arg("--qdrant")
        .arg(directory.join("native/qdrant/qdrant.exe"))
        .arg("--parent-pipe");
    if directory.join("native/edge-preview").is_file() {
        let storage = state.join("vector-edge");
        if !storage.join("les-vector.sqlite").is_file() {
            return Err("Для запуска с Edge сначала перенесите индекс в отдельное хранилище.");
        }
        command.arg("--edge-storage").arg(storage);
    }
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_launch_requires_only_the_native_owner() {
        let root = std::env::temp_dir().join(format!("les-launch-mode-{}", std::process::id()));
        let runtime = root.join("runtime");
        let executable = root.join("native/runtime/les-runtime.exe");
        assert!(command(&root, &runtime, &root).is_err());
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, b"fixture").unwrap();
        let native = command(&root, &runtime, &root).unwrap();
        assert_eq!(native.get_program(), executable);
        let args: Vec<_> = native.get_args().collect();
        assert!(args.contains(&std::ffi::OsStr::new("--parent-pipe")));
        assert!(args.contains(&std::ffi::OsStr::new("--native-only")));
        assert!(!args.contains(&std::ffi::OsStr::new("--python")));
        assert!(args.contains(&std::ffi::OsStr::new("--qdrant")));
        std::fs::write(root.join("native/edge-preview"), []).unwrap();
        assert!(command(&root, &runtime, &root).is_err());
        std::fs::create_dir(root.join("vector-edge")).unwrap();
        std::fs::write(root.join("vector-edge/les-vector.sqlite"), []).unwrap();
        let edge = command(&root, &runtime, &root).unwrap();
        let args: Vec<_> = edge.get_args().collect();
        assert!(args.contains(&std::ffi::OsStr::new("--edge-storage")));
        assert!(args.contains(&std::ffi::OsStr::new("--qdrant")));
        std::fs::remove_dir_all(root).unwrap();
    }
}
