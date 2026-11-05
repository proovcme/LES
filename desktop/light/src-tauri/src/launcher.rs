//! Explicit migration switch. A missing native owner never silently runs Python.
use std::{path::Path,process::Command};

pub fn command(directory:&Path,root:&Path,state:&Path,native:bool)->Result<Command,&'static str> {
    let python=directory.join("python/python.exe");
    if !python.is_file() {return Err("В установке отсутствует обработчик совместимости. Восстановите Лес; документы сохранятся.");}
    let mut command=if native {
        let executable=directory.join("native/runtime/les-runtime.exe");
        if !executable.is_file() {return Err("В установке отсутствует нативная служба запуска. Восстановите приложение.");}
        let mut command=Command::new(executable);command.arg("--python").arg(&python);command
    } else {
        let launcher=root.join("tools/light_launcher.py");
        if !launcher.is_file() {return Err("В установке не хватает компонентов запуска. Восстановите Лес; документы сохранятся.");}
        let mut command=Command::new(python);command.arg("-B").arg(launcher);command
    };
    command.arg("--root").arg(root).arg("--state").arg(state).arg("--qdrant")
        .arg(directory.join("native/qdrant/qdrant.exe")).arg("--parent-pipe");
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn missing_native_owner_cannot_fall_back_to_python_launcher() {
        let root=std::env::temp_dir().join(format!("les-launch-mode-{}",std::process::id()));
        std::fs::create_dir_all(root.join("python")).unwrap();std::fs::create_dir_all(root.join("runtime/tools")).unwrap();
        std::fs::write(root.join("python/python.exe"),b"fixture").unwrap();
        std::fs::write(root.join("runtime/tools/light_launcher.py"),b"fixture").unwrap();
        assert!(command(&root,&root.join("runtime"),&root,false).is_ok());
        assert!(command(&root,&root.join("runtime"),&root,true).is_err());
        std::fs::create_dir_all(root.join("native/runtime")).unwrap();std::fs::write(root.join("native/runtime/les-runtime.exe"),b"fixture").unwrap();
        let native=command(&root,&root.join("runtime"),&root,true).unwrap();
        assert_eq!(native.get_program(),root.join("native/runtime/les-runtime.exe"));
        assert!(native.get_args().any(|arg|arg=="--parent-pipe"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
