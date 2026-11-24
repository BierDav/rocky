mod one_device {
    include!("../../examples/one_device.rs");
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        main()
    }
}

mod refused_write {
    include!("../../examples/refused_write.rs");
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        main()
    }
}

mod two_devices {
    include!("../../examples/two_devices.rs");
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        main()
    }
}

#[test]
fn the_one_device_example_reads_back_its_note() -> Result<(), Box<dyn std::error::Error>> {
    one_device::run()
}

#[test]
fn the_two_device_example_syncs_a_note_over_loopback_tcp() -> Result<(), Box<dyn std::error::Error>>
{
    two_devices::run()
}

#[test]
fn the_refused_write_example_matches_a_clock_value_in_a_replayed_write()
-> Result<(), Box<dyn std::error::Error>> {
    refused_write::run()
}
