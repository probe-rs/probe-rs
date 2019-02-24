// use coresight::access_ports::AccessPortError;
// use memory::MI;
// use memory::ToMemoryReadSize;
// use coresight::memory_interface::ADIMemoryInterface;
// use coresight::access_ports::memory_ap::{
//     CSW,
//     TAR,
//     DRW,
//     MemoryAP,
// };
// use coresight::ap_access::APAccess;

// pub struct STLinkADIMemoryInterface<'a, L>
// where
//     L: APAccess<MemoryAP, CSW> + APAccess<MemoryAP, TAR> + APAccess<MemoryAP, DRW>
// {
//     link: &'a mut L,
//     interface: ADIMemoryInterface,
// }

// impl<'a, L> STLinkADIMemoryInterface<'a, L>
// where
//     L: APAccess<MemoryAP, CSW> + APAccess<MemoryAP, TAR> + APAccess<MemoryAP, DRW>
// {
//     pub fn new(link: &'a mut L) -> Self {
//         Self {
//             link: link,
//             interface: ADIMemoryInterface::new(0),
//         }
//     }
// }

// impl<'a, L> MI<'a> for STLinkADIMemoryInterface<'a, L>
// where
//     L: APAccess<MemoryAP, CSW> + APAccess<MemoryAP, TAR> + APAccess<MemoryAP, DRW>
// {
//     type Error = AccessPortError;

//     fn read<S: ToMemoryReadSize>(&mut self, address: u32) -> Result<S, AccessPortError> {
//         self.interface.read(self.link, address)
//     }

//     fn read_block<S: ToMemoryReadSize>(
//         &mut self,
//         address: u32,
//         data: &mut [S]
//     ) -> Result<(), AccessPortError> {
//         self.interface.read_block(self.link, address, data)
//     }

//     fn write<S: ToMemoryReadSize>(
//         &mut self,
//         addr: u32,
//         data: S
//     ) -> Result<(), AccessPortError> {
//         self.interface.write(self.link, addr, data)
//     }

//     fn write_block<S: ToMemoryReadSize>(
//         &mut self,
//         addr: u32,
//         data: &[S]
//     ) -> Result<(), AccessPortError> {
//         self.interface.write_block(self.link, addr, data)
//     }
// }