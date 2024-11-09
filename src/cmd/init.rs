use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use argp::FromArgs;
use ds_rom::rom::{raw::AutoloadKind, Rom, RomConfig, RomLoadOptions};
use path_slash::PathBufExt;
use pathdiff::diff_paths;

use crate::{
    config::{
        config::{Config, ConfigAutoload, ConfigModule, ConfigOverlay},
        delinks::Delinks,
        module::{Module, ModuleKind},
        program::Program,
        symbol::SymbolMaps,
    },
    util::io::{create_dir_all, create_file, open_file},
};

/// Generates a config for the given extracted ROM.
#[derive(FromArgs)]
#[argp(subcommand, name = "init")]
pub struct Init {
    /// Path to config file in the extract directory.
    #[argp(option, short = 'r')]
    rom_config: PathBuf,

    /// Output path.
    #[argp(option, short = 'o')]
    output_path: PathBuf,

    /// Dry run, do not write files to output path.
    #[argp(switch, short = 'd')]
    dry: bool,

    /// Path to build directory.
    #[argp(option, short = 'b')]
    build_path: PathBuf,
}

impl Init {
    pub fn run(&self) -> Result<()> {
        let rom = Rom::load(
            &self.rom_config,
            RomLoadOptions { compress: false, encrypt: false, load_files: false, ..Default::default() },
        )?;

        let arm9_output_path = self.output_path.join("arm9");
        let arm9_overlays_output_path = arm9_output_path.join("overlays");
        let arm9_config_path = arm9_output_path.join("config.yaml");

        let mut symbol_maps = SymbolMaps::new();

        let main = Module::analyze_arm9(rom.arm9(), &mut symbol_maps)?;
        let overlays =
            rom.arm9_overlays().iter().map(|ov| Module::analyze_overlay(ov, &mut symbol_maps)).collect::<Result<Vec<_>>>()?;
        let autoloads = rom.arm9().autoloads()?;
        let autoloads = autoloads
            .iter()
            .map(|autoload| match autoload.kind() {
                AutoloadKind::Itcm => Module::analyze_itcm(autoload, &mut symbol_maps),
                AutoloadKind::Dtcm => Module::analyze_dtcm(autoload, &mut symbol_maps),
                AutoloadKind::Unknown(_) => bail!("unknown autoload kind"),
            })
            .collect::<Result<Vec<_>>>()?;

        let mut program = Program::new(main, overlays, autoloads, symbol_maps);
        program.analyze_cross_references()?;

        // Generate configs
        let mut rom_config: RomConfig = serde_yml::from_reader(open_file(&self.rom_config)?)?;
        rom_config.arm9_bin = self.build_path.join("build/arm9.bin");
        rom_config.itcm.bin = self.build_path.join("build/itcm.bin");
        rom_config.dtcm.bin = self.build_path.join("build/dtcm.bin");
        rom_config.arm9_overlays = Some(self.build_path.join("build/arm9_overlays.yaml"));
        let rom_config = rom_config;

        let overlay_configs = self.overlay_configs(
            &arm9_output_path,
            &arm9_overlays_output_path,
            program.overlays(),
            "arm9",
            program.symbol_maps(),
        )?;
        let autoload_configs =
            self.autoload_configs(&arm9_output_path, &rom_config, program.autoloads(), program.symbol_maps())?;
        let arm9_config = self.arm9_config(
            &arm9_output_path,
            &rom_config,
            program.main(),
            overlay_configs,
            autoload_configs,
            program.symbol_maps(),
        )?;

        if !self.dry {
            create_dir_all(&arm9_output_path)?;
            serde_yml::to_writer(create_file(arm9_config_path)?, &arm9_config)?;
        }

        Ok(())
    }

    fn make_path<P: AsRef<Path>, B: AsRef<Path>>(path: P, base: B) -> PathBuf {
        PathBuf::from(diff_paths(path, &base).unwrap().to_slash_lossy().as_ref())
    }

    fn arm9_config(
        &self,
        path: &Path,
        rom_config: &RomConfig,
        module: &Module,
        overlays: Vec<ConfigOverlay>,
        autoloads: Vec<ConfigAutoload>,
        symbol_maps: &SymbolMaps,
    ) -> Result<Config> {
        let code_hash = fxhash::hash64(module.code());

        let delinks_path = path.join("delinks.txt");
        let symbols_path = path.join("symbols.txt");
        let relocations_path = path.join("relocs.txt");

        if !self.dry {
            Delinks::to_file(&delinks_path, module.sections())?;
            symbol_maps.get(module.kind()).unwrap().to_file(&symbols_path)?;
            module.relocations().to_file(&relocations_path)?;
        }

        Ok(Config {
            rom_config: Self::make_path(&self.rom_config, path),
            build_path: Self::make_path(&self.build_path, path),
            delinks_path: Self::make_path(&self.build_path.join("delinks"), path),
            main_module: ConfigModule {
                name: "main".to_string(),
                object: Self::make_path(&rom_config.arm9_bin, path),
                hash: format!("{:016x}", code_hash),
                delinks: Self::make_path(delinks_path, path),
                symbols: Self::make_path(symbols_path, path),
                relocations: Self::make_path(relocations_path, path),
            },
            autoloads,
            overlays,
        })
    }

    fn autoload_configs(
        &self,
        path: &Path,
        rom_config: &RomConfig,
        modules: &[Module],
        symbol_maps: &SymbolMaps,
    ) -> Result<Vec<ConfigAutoload>> {
        let mut autoloads = vec![];
        for module in modules {
            let code_hash = fxhash::hash64(module.code());
            let ModuleKind::Autoload(kind) = module.kind() else {
                log::error!("Expected autoload module");
                bail!("Expected autoload module");
            };
            let (name, code_path) = match kind {
                AutoloadKind::Itcm => ("itcm", &rom_config.itcm.bin),
                AutoloadKind::Dtcm => ("dtcm", &rom_config.dtcm.bin),
                _ => {
                    log::error!("Unknown autoload kind");
                    bail!("Unknown autoload kind");
                }
            };

            let autoload_path = path.join(name);
            create_dir_all(&autoload_path)?;

            let delinks_path = autoload_path.join("delinks.txt");
            let symbols_path = autoload_path.join("symbols.txt");
            let relocs_path = autoload_path.join("relocs.txt");

            if !self.dry {
                Delinks::to_file(&delinks_path, module.sections())?;
                symbol_maps.get(module.kind()).unwrap().to_file(&symbols_path)?;
                module.relocations().to_file(&relocs_path)?;
            }

            autoloads.push(ConfigAutoload {
                module: ConfigModule {
                    name: module.name().to_string(),
                    object: Self::make_path(code_path, path),
                    hash: format!("{:016x}", code_hash),
                    delinks: Self::make_path(delinks_path, path),
                    symbols: Self::make_path(symbols_path, path),
                    relocations: Self::make_path(relocs_path, path),
                },
                kind,
            })
        }

        Ok(autoloads)
    }

    fn overlay_configs(
        &self,
        root: &Path,
        path: &Path,
        modules: &[Module],
        processor: &str,
        symbol_maps: &SymbolMaps,
    ) -> Result<Vec<ConfigOverlay>> {
        let mut overlays = vec![];

        for module in modules {
            let ModuleKind::Overlay(id) = module.kind() else {
                log::error!("Expected overlay module");
                bail!("Expected overlay module")
            };

            let code_path = self.build_path.join(format!("build/{processor}_{}.bin", module.name()));
            let code_hash = fxhash::hash64(module.code());

            let overlay_config_path = path.join(module.name());
            create_dir_all(&overlay_config_path)?;

            let delinks_path = overlay_config_path.join("delinks.txt");
            let symbols_path = overlay_config_path.join("symbols.txt");
            let relocs_path = overlay_config_path.join("relocs.txt");

            if !self.dry {
                Delinks::to_file(&delinks_path, module.sections())?;
                symbol_maps.get(module.kind()).unwrap().to_file(&symbols_path)?;
                module.relocations().to_file(&relocs_path)?;
            }

            overlays.push(ConfigOverlay {
                module: ConfigModule {
                    name: module.name().to_string(),
                    object: Self::make_path(code_path, root),
                    hash: format!("{:016x}", code_hash),
                    delinks: Self::make_path(delinks_path, root),
                    symbols: Self::make_path(symbols_path, root),
                    relocations: Self::make_path(relocs_path, root),
                },
                id,
            });
        }

        Ok(overlays)
    }
}
