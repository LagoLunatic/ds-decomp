use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;
use ds_rom::rom::{self, Header, OverlayConfig};

use crate::{
    config::{
        config::{Config, ConfigModule},
        module::Module,
        splits::Splits,
        symbol::SymbolMap,
    },
    util::io::{create_dir_all, create_file, open_file, read_file},
};

/// Generates a config for the given extracted ROM.
#[derive(Debug, Args)]
pub struct Init {
    /// Extraction path.
    #[arg(short = 'e', long)]
    extract_path: PathBuf,

    /// Output path.
    #[arg(short = 'o', long)]
    output_path: PathBuf,
}

impl Init {
    pub fn run(&self) -> Result<()> {
        let header_path = self.extract_path.join("header.yaml");
        let header: Header = serde_yml::from_reader(open_file(header_path)?)?;

        let arm9_output_path = self.output_path.join("arm9");
        let arm9_overlays_output_path = arm9_output_path.join("overlays");
        let arm9_config_path = arm9_output_path.join("config.yaml");

        let arm9_overlays = self.read_overlays(&arm9_overlays_output_path, &header, "arm9")?;
        let arm9_config = self.read_arm9(arm9_overlays)?;

        create_dir_all(&arm9_output_path)?;
        serde_yml::to_writer(create_file(arm9_config_path)?, &arm9_config)?;

        Ok(())
    }

    fn read_arm9(&self, overlays: Vec<ConfigModule>) -> Result<Config> {
        let arm9_bin_file = self.extract_path.join("arm9.bin");
        // let build_config: Arm9BuildConfig = serde_yml::from_reader(File::open(self.extract_path.join("arm9.yaml"))?)?;

        let object_bytes = read_file(&arm9_bin_file)?;
        let object_hash = fxhash::hash64(&object_bytes);

        Ok(Config {
            module: ConfigModule {
                object: arm9_bin_file,
                hash: object_hash,
                splits: "./splits.txt".into(),
                symbols: "./symbols.txt".into(),
                overlay_loads: "./overlay_loads.txt".into(),
            },
            overlays,
        })
    }

    fn read_overlays(&self, path: &Path, header: &Header, processor: &str) -> Result<Vec<ConfigModule>> {
        let mut overlays = vec![];
        let overlays_path = self.extract_path.join(format!("{processor}_overlays"));
        let overlays_config_file = overlays_path.join(format!("{processor}_overlays.yaml"));
        let overlay_configs: Vec<OverlayConfig> = serde_yml::from_reader(open_file(overlays_config_file)?)?;

        for config in overlay_configs {
            let id = config.info.id;

            let data_path = overlays_path.join(config.file_name);
            let data = read_file(&data_path)?;
            let data_hash = fxhash::hash64(&data);

            let symbols = SymbolMap::new();
            let overlay = rom::Overlay::new(data, header.version(), config.info);
            let mut module = Module::new_overlay(symbols, &overlay)?;
            module.find_sections();

            let overlay_config_path = path.join(format!("ov{:03}", id));
            create_dir_all(&overlay_config_path)?;

            let splits_path = overlay_config_path.join("splits.txt");
            Splits::to_file(&splits_path, module.sections())?;

            overlays.push(ConfigModule {
                object: data_path,
                hash: data_hash,
                splits: splits_path,
                symbols: overlay_config_path.join("symbols.txt"),
                overlay_loads: overlay_config_path.join("overlay_loads.txt"),
            });
        }

        Ok(overlays)
    }
}