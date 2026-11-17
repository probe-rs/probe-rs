//! Evaluates DWARF location expressions.

use crate::{DebugError, EndianReader, debug_info::*, stack_frame::StackFrameInfo, variable::*};
use gimli::{AttributeValue, EvaluationResult, Location, UnitOffset};
use probe_rs::{MemoryInterface, RegisterValue};

use super::{RangeExt, UnitInfo};

/// The result of `LocationEvaluator::evaluate_expression()` is a memory location of a variable.
#[derive(Debug)]
pub(crate) enum ExpressionResult {
    Location(VariableLocation),
}

impl UnitInfo {
    pub(crate) fn location_evaluator<'a, 'f>(
        &'a self,
        debug_info: &'a DebugInfo,
        memory: &'a mut dyn MemoryInterface,
        frame_info: &'a StackFrameInfo<'f>,
    ) -> LocationEvaluator<'a, 'f> {
        LocationEvaluator {
            unit: self,
            debug_info,
            memory,
            frame_info,
        }
    }
}

/// Evaluates the location expressions of a unit in a stack frame.
pub(crate) struct LocationEvaluator<'a, 'f> {
    unit: &'a UnitInfo,
    debug_info: &'a DebugInfo,
    memory: &'a mut dyn MemoryInterface,
    frame_info: &'a StackFrameInfo<'f>,
}

impl LocationEvaluator<'_, '_> {
    /// - Find the location using either DW_AT_location, DW_AT_data_member_location, or DW_AT_frame_base attribute.
    ///
    /// Return values are implemented as follows:
    /// - `Result<_, DebugError>`: This happens when we encounter an error we did not expect, and will propagate upwards until the debugger request is failed. **NOT GRACEFUL**, and should be avoided.
    /// - `Result<ExpressionResult::Location(),_>`: One of the variants of VariableLocation, and needs to be interpreted for handling the 'expected' errors we encounter during evaluation.
    pub(crate) fn extract_location(
        &mut self,
        node_die: &gimli::DebuggingInformationEntry<GimliReader>,
        parent_location: &VariableLocation,
        byte_size: Option<u64>,
    ) -> Result<ExpressionResult, DebugError> {
        for attr in node_die.attrs() {
            match attr.name() {
                gimli::DW_AT_location
                | gimli::DW_AT_frame_base
                | gimli::DW_AT_data_member_location => {
                    return self.location_attribute(attr.value(), parent_location, byte_size);
                }
                gimli::DW_AT_address_class => {
                    let location = address_class_location(attr.value(), parent_location);
                    return Ok(ExpressionResult::Location(location));
                }
                _other_attributes => {
                    // These will be handled elsewhere.
                }
            }
        }

        // If we get here, we did not find a location attribute, then leave the value as Unknown.
        Ok(ExpressionResult::Location(VariableLocation::Unknown))
    }

    fn location_attribute(
        &mut self,
        value: AttributeValue<GimliReader>,
        parent_location: &VariableLocation,
        byte_size: Option<u64>,
    ) -> Result<ExpressionResult, DebugError> {
        match value {
            AttributeValue::Exprloc(expression) => {
                convert_incomplete(self.evaluate_expression(expression))
            }
            AttributeValue::Udata(offset_from_location) => Ok(ExpressionResult::Location(
                parent_location.offset_by(offset_from_location, byte_size),
            )),
            other_attribute_value => self.location_list_attribute(other_attribute_value),
        }
    }

    fn location_list_attribute(
        &mut self,
        value: AttributeValue<GimliReader>,
    ) -> Result<ExpressionResult, DebugError> {
        let location = match self
            .debug_info
            .dwarf
            .attr_locations_offset(&self.unit.unit, value.clone())
        {
            Ok(Some(location_list_offset)) => {
                return convert_incomplete(self.evaluate_location_list(location_list_offset));
            }
            Ok(None) => VariableLocation::Unsupported(format!(
                "Unimplemented: extract_location() Could not extract location from: {:.100}",
                format!("{value:?}")
            )),
            Err(error) => {
                VariableLocation::Error(format!("Error: Resolving variable Location: {error:?}"))
            }
        };
        Ok(ExpressionResult::Location(location))
    }

    fn evaluate_location_list(
        &mut self,
        location_list_offset: gimli::LocationListsOffset,
    ) -> Result<ExpressionResult, DebugError> {
        let mut locations = match self
            .debug_info
            .dwarf
            .locations(&self.unit.unit, location_list_offset)
        {
            Ok(locations) => locations,
            Err(error) => {
                return Ok(ExpressionResult::Location(VariableLocation::Error(
                    format!("Error: Resolving variable Location: {error:?}"),
                )));
            }
        };
        let Some(program_counter) = self
            .frame_info
            .registers
            .get_program_counter()
            .and_then(|reg| reg.value)
        else {
            return Ok(ExpressionResult::Location(VariableLocation::Error(
                "Cannot determine variable location without a valid program counter.".to_string(),
            )));
        };
        let program_counter: Option<u64> = program_counter.try_into().ok();

        loop {
            match locations.next() {
                Ok(Some(location))
                    if program_counter.is_some_and(|pc| location.range.contains(pc)) =>
                {
                    return self.evaluate_expression(location.data);
                }
                Ok(Some(_)) => {}
                Ok(None) => return Ok(ExpressionResult::Location(VariableLocation::Unavailable)),
                Err(error) => {
                    return Ok(ExpressionResult::Location(VariableLocation::Error(
                        format!("Error while iterating LocationLists for this variable: {error:?}"),
                    )));
                }
            }
        }
    }

    /// Evaluate a [`gimli::Expression`] as a valid memory location.
    /// Return values are implemented as follows:
    /// - `Result<_, DebugError>`: This happens when we encounter an error we did not expect, and will propagate upwards until the debugger request is failed. NOT GRACEFUL, and should be avoided.
    /// - `Result<ExpressionResult::Location(),_>`: One of the variants of VariableLocation, and needs to be interpreted for handling the 'expected' errors we encounter during evaluation.
    pub(crate) fn evaluate_expression(
        &mut self,
        expression: gimli::Expression<GimliReader>,
    ) -> Result<ExpressionResult, DebugError> {
        let pieces = self.expression_to_pieces(expression, 0)?;

        let location = match &pieces[..] {
            [] => VariableLocation::Error("Error: expr_to_piece() returned 0 results".to_string()),
            [piece] => match self.piece_location(piece) {
                Some(location) => location,
                None => self.assemble_pieces(&pieces)?,
            },
            _ => self.assemble_pieces(&pieces)?,
        };

        Ok(ExpressionResult::Location(location))
    }

    /// The location of a value that one piece holds. `None` if the piece does not start at a byte
    /// boundary, or holds a part of a byte: then the bits of the piece need to be assembled.
    fn piece_location(
        &mut self,
        piece: &gimli::Piece<GimliReader, usize>,
    ) -> Option<VariableLocation> {
        let bit_offset = piece.bit_offset.unwrap_or(0);
        let aligned =
            bit_offset.is_multiple_of(8) && piece.size_in_bits.is_none_or(|bits| bits % 8 == 0);

        let location = match &piece.location {
            // This means the value was optimized away.
            Location::Empty => VariableLocation::Unavailable,
            Location::Address { address: 0 } => {
                let error = "The value of this variable may have been optimized out of the debug info, by the compiler.";
                VariableLocation::Error(error.to_string())
            }
            Location::Address { address } if aligned => {
                self.address_location(address + bit_offset / 8)
            }
            Location::Register { register } if aligned && bit_offset == 0 => {
                match self.register_value(*register) {
                    Some(value) => VariableLocation::RegisterValue(value),
                    None => VariableLocation::Error(format!(
                        "Error: Cannot resolve register: {register:?}"
                    )),
                }
            }
            _partial_piece => return None,
        };
        Some(location)
    }

    fn address_location(&mut self, address: u64) -> VariableLocation {
        if address >= u32::MAX as u64 && !self.memory.supports_native_64bit_access() {
            VariableLocation::Error(format!(
                "The memory location for this variable value ({address:#010X}) is invalid. Please report this as a bug."
            ))
        } else {
            VariableLocation::Address(address)
        }
    }

    fn register_value(&self, register: gimli::Register) -> Option<RegisterValue> {
        self.frame_info
            .registers
            .get_register_by_dwarf_id(register.0)
            .and_then(|register| register.value)
    }

    /// Describe a value that is assembled from more than one place, or from a part of a place.
    fn assemble_pieces(
        &self,
        pieces: &[gimli::Piece<GimliReader, usize>],
    ) -> Result<VariableLocation, DebugError> {
        let mut location_pieces = Vec::with_capacity(pieces.len());

        for piece in pieces {
            let source = match &piece.location {
                Location::Empty => PieceSource::Empty,
                Location::Address { address } => PieceSource::Address(*address),
                Location::Register { register } => {
                    let Some(value) = self.register_value(*register) else {
                        return Ok(VariableLocation::Error(format!(
                            "Error: Cannot resolve register: {register:?}"
                        )));
                    };

                    PieceSource::Register(value)
                }
                Location::Value { value } => PieceSource::Implicit(value_bytes(*value)),
                Location::Bytes { value } => {
                    PieceSource::Implicit(gimli::Reader::to_slice(value)?.to_vec())
                }
                location @ Location::ImplicitPointer { .. } => {
                    return Ok(VariableLocation::Unsupported(format!(
                        "Unimplemented: extract_location() found a location type: {:.100}",
                        format!("{location:?}")
                    )));
                }
            };

            location_pieces.push(LocationPiece {
                source,
                bit_offset: piece.bit_offset.unwrap_or(0),
                bit_size: piece.size_in_bits,
            });
        }

        Ok(VariableLocation::Composite(location_pieces))
    }

    /// Tries to get the result of a DWARF expression in the form of pieces.
    fn expression_to_pieces(
        &mut self,
        expression: gimli::Expression<GimliReader>,
        entry_value_depth: u8,
    ) -> Result<Vec<gimli::Piece<GimliReader, usize>>, DebugError> {
        Ok(self.evaluate(expression, entry_value_depth)?.result())
    }

    fn expression_to_value(
        &mut self,
        expression: gimli::Expression<GimliReader>,
        entry_value_depth: u8,
    ) -> Result<gimli::Value, DebugError> {
        let evaluation = self.evaluate(expression, entry_value_depth)?;
        match evaluation.value_result() {
            Some(value) => Ok(value),
            None => self.pieces_to_value(&evaluation.result()),
        }
    }

    /// Runs `expression` to completion, and provides the memory, registers and frame addresses
    /// that it requests.
    fn evaluate(
        &mut self,
        expression: gimli::Expression<GimliReader>,
        entry_value_depth: u8,
    ) -> Result<gimli::Evaluation<GimliReader>, DebugError> {
        let mut evaluation = expression.evaluation(self.unit.unit.encoding());
        let mut result = evaluation.evaluate()?;

        loop {
            result = match result {
                EvaluationResult::Complete => return Ok(evaluation),
                EvaluationResult::RequiresMemory { address, size, .. } => {
                    read_memory(size, self.memory, address, &mut evaluation)?
                }
                EvaluationResult::RequiresFrameBase => provide_frame_address(
                    self.frame_info.frame_base,
                    "frame base address",
                    |frame_base| evaluation.resume_with_frame_base(frame_base),
                )?,
                EvaluationResult::RequiresRegister {
                    register,
                    base_type,
                } => self.provide_register(register, base_type, &mut evaluation)?,
                EvaluationResult::RequiresRelocatedAddress(address_index) => {
                    // The address_index as an offset from 0, so just pass it into the next step.
                    evaluation.resume_with_relocated_address(address_index)?
                }
                EvaluationResult::RequiresCallFrameCfa => provide_frame_address(
                    self.frame_info.canonical_frame_address,
                    "canonical frame address",
                    |cfa| evaluation.resume_with_call_frame_cfa(cfa),
                )?,
                EvaluationResult::RequiresEntryValue(inner) => {
                    let value = self.resolve_entry_value(inner, entry_value_depth)?;
                    evaluation.resume_with_entry_value(value)?
                }
                unimplemented_expression => {
                    return Err(DebugError::WarnAndContinue {
                        message: unsupported_evaluation_result(&unimplemented_expression),
                    });
                }
            }
        }
    }

    fn resolve_entry_value(
        &mut self,
        inner: gimli::Expression<GimliReader>,
        entry_value_depth: u8,
    ) -> Result<gimli::Value, DebugError> {
        if entry_value_depth >= 4 {
            return Err(DebugError::WarnAndContinue {
                message: "Nested DW_OP_entry_value is not supported.".to_string(),
            });
        }

        let Some(caller) = self.frame_info.caller else {
            return Err(DebugError::WarnAndContinue {
                message: "DW_OP_entry_value requires a caller frame.".to_string(),
            });
        };

        let call_site = caller
            .program_counter()
            .and_then(|pc| self.debug_info.call_site_value(pc, &inner));
        let (unit, expression) = call_site.unwrap_or((self.unit, inner));

        unit.location_evaluator(self.debug_info, self.memory, caller)
            .expression_to_value(expression, entry_value_depth + 1)
    }

    fn pieces_to_value(
        &self,
        pieces: &[gimli::Piece<GimliReader, usize>],
    ) -> Result<gimli::Value, DebugError> {
        let [piece] = pieces else {
            return Err(DebugError::WarnAndContinue {
                message: "DW_OP_entry_value produced a composite location.".to_string(),
            });
        };

        match &piece.location {
            Location::Value { value } => Ok(*value),
            Location::Register { register } => {
                let Some(raw_value) = self.register_value(*register) else {
                    return Err(DebugError::WarnAndContinue {
                        message: format!(
                            "DW_OP_entry_value has no value for register #:{}.",
                            register.0
                        ),
                    });
                };
                Ok(gimli::Value::Generic(raw_value.try_into()?))
            }
            Location::Address { address } => Ok(gimli::Value::Generic(*address)),
            Location::Bytes { value } => {
                let bytes = gimli::Reader::to_slice(value)?;
                let mut buf = [0u8; 8];
                let len = bytes.len().min(8);
                buf[..len].copy_from_slice(&bytes[..len]);
                Ok(gimli::Value::Generic(u64::from_le_bytes(buf)))
            }
            Location::Empty => Err(DebugError::WarnAndContinue {
                message: "DW_OP_entry_value produced an empty location.".to_string(),
            }),
            Location::ImplicitPointer { .. } => Err(DebugError::WarnAndContinue {
                message: "DW_OP_entry_value produced an implicit pointer.".to_string(),
            }),
        }
    }

    /// Gets necessary register information for the DWARF resolver.
    fn provide_register(
        &self,
        register: gimli::Register,
        base_type: UnitOffset,
        evaluation: &mut gimli::Evaluation<EndianReader>,
    ) -> Result<EvaluationResult<EndianReader>, DebugError> {
        match self.register_value(register) {
            Some(raw_value) if base_type == gimli::UnitOffset(0) => {
                let register_value = gimli::Value::Generic(raw_value.try_into()?);
                Ok(evaluation.resume_with_register(register_value)?)
            }
            Some(_) => Err(DebugError::WarnAndContinue {
                message: format!(
                    "Unimplemented: Support for type {base_type:?} in `RequiresRegister`"
                ),
            }),
            None => Err(DebugError::WarnAndContinue {
                message: format!(
                    "Error while calculating `Variable::memory_location`. No value for register #:{}.",
                    register.0
                ),
            }),
        }
    }
}

/// Turns UnwindIncompleteResults into Unavailable locations
fn convert_incomplete(
    result: Result<ExpressionResult, DebugError>,
) -> Result<ExpressionResult, DebugError> {
    match result {
        Err(DebugError::WarnAndContinue { message }) => {
            tracing::warn!("UnwindIncompleteResults: {:?}", message);
            Ok(ExpressionResult::Location(VariableLocation::Unavailable))
        }
        other => other,
    }
}

fn address_class_location(
    value: AttributeValue<GimliReader>,
    parent_location: &VariableLocation,
) -> VariableLocation {
    match value {
        AttributeValue::AddressClass(gimli::DwAddr(0)) => {
            // We pass on the location of the parent, which will later to be used along with DW_AT_data_member_location to calculate the location of this variable.
            parent_location.clone()
        }
        AttributeValue::AddressClass(address_class) => VariableLocation::Unsupported(format!(
            "Unimplemented: extract_location() found unsupported DW_AT_address_class(gimli::DwAddr({address_class:?}))"
        )),
        other_attribute_value => VariableLocation::Unsupported(format!(
            "Unimplemented: extract_location() found invalid DW_AT_address_class: {:.100}",
            format!("{other_attribute_value:?}")
        )),
    }
}

fn unsupported_evaluation_result<R: gimli::Reader>(result: &EvaluationResult<R>) -> String {
    let kind = match result {
        EvaluationResult::RequiresTls(_) => "DW_OP_form_tls_address",
        EvaluationResult::RequiresAtLocation(_) => "DW_OP_call",
        EvaluationResult::RequiresParameterRef(_) => "DW_OP_parameter_ref",
        EvaluationResult::RequiresIndexedAddress { .. } => "an indexed address",
        EvaluationResult::RequiresBaseType(_) => "a typed DWARF value",
        EvaluationResult::RequiresEntryValue(_) => "DW_OP_entry_value",
        _ => "this DWARF evaluation request",
    };
    format!("Unimplemented: {kind} is not currently supported.")
}

/// Gets the frame base or the canonical frame address, which `name` names, for the DWARF
/// resolver.
fn provide_frame_address(
    address: Option<u64>,
    name: &str,
    resume: impl FnOnce(u64) -> gimli::Result<EvaluationResult<EndianReader>>,
) -> Result<EvaluationResult<EndianReader>, DebugError> {
    let Some(address) = address else {
        return Err(DebugError::WarnAndContinue {
            message: format!("Cannot unwind `Variable` location without a valid {name}.)"),
        });
    };
    resume(address).map_err(|error| DebugError::WarnAndContinue {
        message: format!("Error while calculating `Variable::memory_location`:{error}."),
    })
}

/// The bytes of a value that the debug info holds, in little endian order.
fn value_bytes(value: gimli::Value) -> Vec<u8> {
    match value {
        gimli::Value::Generic(value) => value.to_le_bytes().to_vec(),
        gimli::Value::I8(value) => value.to_le_bytes().to_vec(),
        gimli::Value::U8(value) => value.to_le_bytes().to_vec(),
        gimli::Value::I16(value) => value.to_le_bytes().to_vec(),
        gimli::Value::U16(value) => value.to_le_bytes().to_vec(),
        gimli::Value::I32(value) => value.to_le_bytes().to_vec(),
        gimli::Value::U32(value) => value.to_le_bytes().to_vec(),
        gimli::Value::I64(value) => value.to_le_bytes().to_vec(),
        gimli::Value::U64(value) => value.to_le_bytes().to_vec(),
        gimli::Value::F32(value) => value.to_le_bytes().to_vec(),
        gimli::Value::F64(value) => value.to_le_bytes().to_vec(),
    }
}

/// Reads memory requested by the DWARF resolver.
fn read_memory(
    size: u8,
    memory: &mut dyn MemoryInterface,
    address: u64,
    evaluation: &mut gimli::Evaluation<EndianReader>,
) -> Result<EvaluationResult<EndianReader>, DebugError> {
    /// Reads `SIZE` bytes from the memory.
    fn read<const SIZE: usize>(
        memory: &mut dyn MemoryInterface,
        address: u64,
    ) -> Result<[u8; SIZE], DebugError> {
        let mut buff = [0u8; SIZE];
        memory.read(address, &mut buff).map_err(|error| {
            DebugError::WarnAndContinue {
                message: format!("Unexpected error while reading debug expressions from target memory: {error:?}. Please report this as a bug.")
            }
        })?;
        Ok(buff)
    }

    let val = match size {
        1 => {
            let buff = read::<1>(memory, address)?;
            gimli::Value::U8(buff[0])
        }
        2 => {
            let buff = read::<2>(memory, address)?;
            gimli::Value::U16(u16::from_le_bytes(buff))
        }
        4 => {
            let buff = read::<4>(memory, address)?;
            gimli::Value::U32(u32::from_le_bytes(buff))
        }
        8 => {
            let buff = read::<8>(memory, address)?;
            gimli::Value::U64(u64::from_le_bytes(buff))
        }
        x => {
            return Err(DebugError::WarnAndContinue {
                message: format!(
                    "Unimplemented: Requested memory with size {x}, which is not supported yet."
                ),
            });
        }
    };

    Ok(evaluation.resume_with_memory(val)?)
}
