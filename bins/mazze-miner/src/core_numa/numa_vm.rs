use parking_lot::RwLock;
use std::sync::Arc;

pub struct NumaAwareVM {
    vm: Arc<RwLock<RandomXVM>>,
    cache: Arc<RwLock<RandomXCache>>,
    node_id: NodeId,
    flags: RandomXFlag,
}

impl NumaAwareVM {
    pub fn new(node: NodeId) -> Result<Self, NumaError> {
        // Set thread affinity for VM creation
        numa::run_on_node(node.0)
            .map_err(|_| NumaError::ThreadAffinityFailed)?;

        let flags = Self::get_optimal_flags()?;
        let cache = Arc::new(RwLock::new(
            RandomXCache::new(flags, &[0; 32])
                .map_err(NumaError::RandomX)?
        ));

        let vm = Arc::new(RwLock::new(
            RandomXVM::new(flags, Some(cache.read().clone()), None)
                .map_err(NumaError::RandomX)?
        ));

        Ok(Self {
            vm,
            cache,
            node_id: node,
            flags,
        })
    }

    fn get_optimal_flags() -> Result<RandomXFlag, NumaError> {
        let mut flags = RandomXFlag::get_recommended_flags();
        
        // Only enable if we have enough memory per NUMA node
        if Self::check_memory_available()? {
            flags |= RandomXFlag::FLAG_FULL_MEM;
        }

        Ok(flags)
    }

    pub fn check_node_memory(node_id: NodeId) -> Result<bool, NumaError> {
        #[cfg(target_os = "linux")]
        {
            let meminfo = std::fs::read_to_string("/proc/meminfo")
                .map_err(|_| NumaError::MemoryCheckFailed)?;
            
            // Parse MemAvailable in kB
            let available = meminfo.lines()
                .find(|line| line.starts_with("MemAvailable:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|kb_str| kb_str.parse::<u64>().ok())
                .ok_or(NumaError::MemoryParseError)?;
    
            // Convert kB to GB
            let available_gb = available / (1024 * 1024);
            Ok(available_gb >= REQUIRED_MEMORY_GB)
        }
    
        #[cfg(not(target_os = "linux"))]
        Ok(false)
    }
}