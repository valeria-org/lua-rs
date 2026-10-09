//! C type management.
//!
//! Port of lj_ctype.c/lj_ctype.h (x86-64, GC64). Element names are byte strings
//! interned in a name table owned by the [`CTState`] (upstream uses fixed
//! `GCstr`s); comparisons are by name id, i.e. by identity like upstream.

use std::collections::HashMap;

use super::callback::CallbackState;
use super::{FfiError, errmsg};

pub type CTInfo = u32;
pub type CTSize = u32;
pub type CTypeID = u32;
pub type CTypeID1 = u16;

// -- C type numbers (highest 4 bits of C type info). ORDER CT. ------------

pub const CT_NUM: u32 = 0;
pub const CT_STRUCT: u32 = 1;
pub const CT_PTR: u32 = 2;
pub const CT_ARRAY: u32 = 3;
pub const CT_MAYCONVERT: u32 = CT_ARRAY;
pub const CT_VOID: u32 = 4;
pub const CT_ENUM: u32 = 5;
pub const CT_HASSIZE: u32 = CT_ENUM;
pub const CT_FUNC: u32 = 6;
pub const CT_TYPEDEF: u32 = 7;
pub const CT_ATTRIB: u32 = 8;
pub const CT_FIELD: u32 = 9;
pub const CT_BITFIELD: u32 = 10;
pub const CT_CONSTVAL: u32 = 11;
pub const CT_EXTERN: u32 = 12;
pub const CT_KW: u32 = 13;

// -- C type info flags. ----------------------------------------------------

pub const CTF_BOOL: u32 = 0x0800_0000;
pub const CTF_FP: u32 = 0x0400_0000;
pub const CTF_CONST: u32 = 0x0200_0000;
pub const CTF_VOLATILE: u32 = 0x0100_0000;
pub const CTF_UNSIGNED: u32 = 0x0080_0000;
pub const CTF_LONG: u32 = 0x0040_0000;
pub const CTF_VLA: u32 = 0x0010_0000;
pub const CTF_REF: u32 = 0x0080_0000;
pub const CTF_VECTOR: u32 = 0x0800_0000;
pub const CTF_COMPLEX: u32 = 0x0400_0000;
pub const CTF_UNION: u32 = 0x0080_0000;
pub const CTF_VARARG: u32 = 0x0080_0000;
pub const CTF_SSEREGPARM: u32 = 0x0040_0000;

pub const CTF_QUAL: u32 = CTF_CONST | CTF_VOLATILE;
pub const CTF_ALIGN: u32 = CTMASK_ALIGN << CTSHIFT_ALIGN;
/// `char` is signed on x86-64.
pub const CTF_UCHAR: u32 = 0;

/// Parser flags in `attr`.
pub const CTFP_ALIGNED: u32 = 0x0000_0001;
pub const CTFP_PACKED: u32 = 0x0000_0002;
/// Parser flag in `fattr`.
pub const CTFP_CCONV: u32 = 0x0000_0001;

// -- C type info bitfields. ------------------------------------------------

pub const CTMASK_CID: u32 = 0x0000_ffff;
pub const CTMASK_NUM: u32 = 0xf000_0000;
pub const CTSHIFT_NUM: u32 = 28;
pub const CTMASK_ALIGN: u32 = 15;
pub const CTSHIFT_ALIGN: u32 = 16;
pub const CTMASK_ATTRIB: u32 = 255;
pub const CTSHIFT_ATTRIB: u32 = 16;
pub const CTMASK_CCONV: u32 = 3;
pub const CTSHIFT_CCONV: u32 = 16;
pub const CTMASK_REGPARM: u32 = 3;
pub const CTSHIFT_REGPARM: u32 = 18;
pub const CTMASK_VSIZEP: u32 = 15;
pub const CTSHIFT_VSIZEP: u32 = 4;
pub const CTMASK_MSIZEP: u32 = 255;
pub const CTSHIFT_MSIZEP: u32 = 8;

pub const CTBSZ_MAX: u32 = 32;
pub const CTBSZ_FIELD: u32 = 127;
pub const CTMASK_BITPOS: u32 = 127;
pub const CTMASK_BITBSZ: u32 = 127;
pub const CTMASK_BITCSZ: u32 = 127;
pub const CTSHIFT_BITPOS: u32 = 0;
pub const CTSHIFT_BITBSZ: u32 = 8;
pub const CTSHIFT_BITCSZ: u32 = 16;

/// `CTF_INSERT(info, field, val)`: replace the bitfield `mask << shift` of `info`.
#[inline]
pub const fn ctf_insert(info: u32, mask: u32, shift: u32, val: u32) -> u32 {
    (info & !(mask << shift)) | ((val & mask) << shift)
}

// Calling conventions. ORDER CC
pub const CTCC_CDECL: u32 = 0;
pub const CTCC_THISCALL: u32 = 1;
pub const CTCC_FASTCALL: u32 = 2;
pub const CTCC_STDCALL: u32 = 3;

// Attribute numbers.
pub const CTA_NONE: u32 = 0;
pub const CTA_QUAL: u32 = 1;
pub const CTA_ALIGN: u32 = 2;
pub const CTA_SUBTYPE: u32 = 3;
pub const CTA_REDIR: u32 = 4;
pub const CTA_BAD: u32 = 5;

pub const CTSIZE_INVALID: u32 = 0xffff_ffff;

pub const CTHASH_SIZE: usize = 128;
pub const CTHASH_MASK: u32 = CTHASH_SIZE as u32 - 1;

pub const CTSIZE_PTR: u32 = 8;
pub const CTALIGN_PTR: u32 = ctalign(3);
pub const CT_MEMALIGN: u32 = 3;

#[inline]
pub const fn ctinfo(ct: u32, flags: u32) -> CTInfo {
    (ct << CTSHIFT_NUM).wrapping_add(flags)
}
#[inline]
pub const fn ctalign(al: u32) -> u32 {
    al << CTSHIFT_ALIGN
}
#[inline]
pub const fn ctattrib(at: u32) -> u32 {
    at << CTSHIFT_ATTRIB
}
#[inline]
pub const fn ctinfo_ref(r: CTypeID) -> CTInfo {
    ctinfo(CT_PTR, (CTF_CONST | CTF_REF | CTALIGN_PTR) + r)
}

#[inline]
pub const fn ctype_type(info: CTInfo) -> u32 {
    info >> CTSHIFT_NUM
}
#[inline]
pub const fn ctype_cid(info: CTInfo) -> CTypeID {
    info & CTMASK_CID
}
#[inline]
pub const fn ctype_align(info: CTInfo) -> u32 {
    (info >> CTSHIFT_ALIGN) & CTMASK_ALIGN
}
#[inline]
pub const fn ctype_attrib(info: CTInfo) -> u32 {
    (info >> CTSHIFT_ATTRIB) & CTMASK_ATTRIB
}
#[inline]
pub const fn ctype_bitpos(info: CTInfo) -> u32 {
    (info >> CTSHIFT_BITPOS) & CTMASK_BITPOS
}
#[inline]
pub const fn ctype_bitbsz(info: CTInfo) -> u32 {
    (info >> CTSHIFT_BITBSZ) & CTMASK_BITBSZ
}
#[inline]
pub const fn ctype_bitcsz(info: CTInfo) -> u32 {
    (info >> CTSHIFT_BITCSZ) & CTMASK_BITCSZ
}
#[inline]
pub const fn ctype_vsize_p(info: CTInfo) -> u32 {
    (info >> CTSHIFT_VSIZEP) & CTMASK_VSIZEP
}
#[inline]
pub const fn ctype_msize_p(info: CTInfo) -> u32 {
    (info >> CTSHIFT_MSIZEP) & CTMASK_MSIZEP
}
#[inline]
pub const fn ctype_cconv(info: CTInfo) -> u32 {
    (info >> CTSHIFT_CCONV) & CTMASK_CCONV
}

#[inline]
pub const fn ctype_isnum(info: CTInfo) -> bool {
    ctype_type(info) == CT_NUM
}
#[inline]
pub const fn ctype_isvoid(info: CTInfo) -> bool {
    ctype_type(info) == CT_VOID
}
#[inline]
pub const fn ctype_isptr(info: CTInfo) -> bool {
    ctype_type(info) == CT_PTR
}
#[inline]
pub const fn ctype_isarray(info: CTInfo) -> bool {
    ctype_type(info) == CT_ARRAY
}
#[inline]
pub const fn ctype_isstruct(info: CTInfo) -> bool {
    ctype_type(info) == CT_STRUCT
}
#[inline]
pub const fn ctype_isfunc(info: CTInfo) -> bool {
    ctype_type(info) == CT_FUNC
}
#[inline]
pub const fn ctype_isenum(info: CTInfo) -> bool {
    ctype_type(info) == CT_ENUM
}
#[inline]
pub const fn ctype_istypedef(info: CTInfo) -> bool {
    ctype_type(info) == CT_TYPEDEF
}
#[inline]
pub const fn ctype_isattrib(info: CTInfo) -> bool {
    ctype_type(info) == CT_ATTRIB
}
#[inline]
pub const fn ctype_isfield(info: CTInfo) -> bool {
    ctype_type(info) == CT_FIELD
}
#[inline]
pub const fn ctype_isbitfield(info: CTInfo) -> bool {
    ctype_type(info) == CT_BITFIELD
}
#[inline]
pub const fn ctype_isconstval(info: CTInfo) -> bool {
    ctype_type(info) == CT_CONSTVAL
}
#[inline]
pub const fn ctype_isextern(info: CTInfo) -> bool {
    ctype_type(info) == CT_EXTERN
}
#[inline]
pub const fn ctype_hassize(info: CTInfo) -> bool {
    ctype_type(info) <= CT_HASSIZE
}
#[inline]
pub const fn ctype_isinteger(info: CTInfo) -> bool {
    (info & (CTMASK_NUM | CTF_BOOL | CTF_FP)) == ctinfo(CT_NUM, 0)
}
#[inline]
pub const fn ctype_isinteger_or_bool(info: CTInfo) -> bool {
    (info & (CTMASK_NUM | CTF_FP)) == ctinfo(CT_NUM, 0)
}
#[inline]
pub const fn ctype_isbool(info: CTInfo) -> bool {
    (info & (CTMASK_NUM | CTF_BOOL)) == ctinfo(CT_NUM, CTF_BOOL)
}
#[inline]
pub const fn ctype_isfp(info: CTInfo) -> bool {
    (info & (CTMASK_NUM | CTF_FP)) == ctinfo(CT_NUM, CTF_FP)
}
/// Pointer or array.
#[inline]
pub const fn ctype_ispointer(info: CTInfo) -> bool {
    (ctype_type(info) >> 1) == (CT_PTR >> 1)
}
#[inline]
pub const fn ctype_isref(info: CTInfo) -> bool {
    (info & (CTMASK_NUM | CTF_REF)) == ctinfo(CT_PTR, CTF_REF)
}
#[inline]
pub const fn ctype_isrefarray(info: CTInfo) -> bool {
    (info & (CTMASK_NUM | CTF_VECTOR | CTF_COMPLEX)) == ctinfo(CT_ARRAY, 0)
}
#[inline]
pub const fn ctype_isvector(info: CTInfo) -> bool {
    (info & (CTMASK_NUM | CTF_VECTOR)) == ctinfo(CT_ARRAY, CTF_VECTOR)
}
#[inline]
pub const fn ctype_iscomplex(info: CTInfo) -> bool {
    (info & (CTMASK_NUM | CTF_COMPLEX)) == ctinfo(CT_ARRAY, CTF_COMPLEX)
}
/// VL array or VL struct.
#[inline]
pub const fn ctype_isvltype(info: CTInfo) -> bool {
    (info & ((CTMASK_NUM | CTF_VLA) - (2u32 << CTSHIFT_NUM))) == ctinfo(CT_STRUCT, CTF_VLA)
}
#[inline]
pub const fn ctype_isvlarray(info: CTInfo) -> bool {
    (info & (CTMASK_NUM | CTF_VLA)) == ctinfo(CT_ARRAY, CTF_VLA)
}
#[inline]
pub const fn ctype_isxattrib(info: CTInfo, at: u32) -> bool {
    (info & (CTMASK_NUM | ctattrib(CTMASK_ATTRIB))) == ctinfo(CT_ATTRIB, ctattrib(at))
}

// -- Predefined types (CTTYDEF order). -------------------------------------

pub const CTID_NONE: CTypeID = 0;
pub const CTID_VOID: CTypeID = 1;
pub const CTID_CVOID: CTypeID = 2;
pub const CTID_BOOL: CTypeID = 3;
pub const CTID_CCHAR: CTypeID = 4;
pub const CTID_INT8: CTypeID = 5;
pub const CTID_UINT8: CTypeID = 6;
pub const CTID_INT16: CTypeID = 7;
pub const CTID_UINT16: CTypeID = 8;
pub const CTID_INT32: CTypeID = 9;
pub const CTID_UINT32: CTypeID = 10;
pub const CTID_INT64: CTypeID = 11;
pub const CTID_UINT64: CTypeID = 12;
pub const CTID_INT128: CTypeID = 13;
pub const CTID_UINT128: CTypeID = 14;
pub const CTID_FLOAT: CTypeID = 15;
pub const CTID_DOUBLE: CTypeID = 16;
pub const CTID_COMPLEX_FLOAT: CTypeID = 17;
pub const CTID_COMPLEX_DOUBLE: CTypeID = 18;
pub const CTID_P_VOID: CTypeID = 19;
pub const CTID_P_CVOID: CTypeID = 20;
pub const CTID_P_CCHAR: CTypeID = 21;
pub const CTID_P_UINT8: CTypeID = 22;
pub const CTID_A_CCHAR: CTypeID = 23;
pub const CTID_CTYPEID: CTypeID = 24;
/// One past the last C type ID.
pub const CTID_MAX: CTypeID = 65536;

pub const CTID_INT_PSZ: CTypeID = CTID_INT64;
pub const CTID_UINT_PSZ: CTypeID = CTID_UINT64;
pub const CTID_WCHAR: CTypeID = CTID_INT32;

/// `CTF_LONG_IF8`: `long` is 8 bytes on x86-64 Linux.
const CTF_LONG_IF8: u32 = CTF_LONG;

/// CTTYDEF: (size, info) of the predefined types, indexed by CTypeID.
const CTTYDEF: [(CTSize, CTInfo); 25] = [
    (0, ctinfo(CT_ATTRIB, ctattrib(CTA_BAD))),
    (CTSIZE_INVALID, ctinfo(CT_VOID, ctalign(0))),
    (CTSIZE_INVALID, ctinfo(CT_VOID, CTF_CONST | ctalign(0))),
    (1, ctinfo(CT_NUM, CTF_BOOL | CTF_UNSIGNED | ctalign(0))),
    (1, ctinfo(CT_NUM, CTF_CONST | CTF_UCHAR | ctalign(0))),
    (1, ctinfo(CT_NUM, ctalign(0))),
    (1, ctinfo(CT_NUM, CTF_UNSIGNED | ctalign(0))),
    (2, ctinfo(CT_NUM, ctalign(1))),
    (2, ctinfo(CT_NUM, CTF_UNSIGNED | ctalign(1))),
    (4, ctinfo(CT_NUM, ctalign(2))),
    (4, ctinfo(CT_NUM, CTF_UNSIGNED | ctalign(2))),
    (8, ctinfo(CT_NUM, CTF_LONG_IF8 | ctalign(3))),
    (8, ctinfo(CT_NUM, CTF_UNSIGNED | CTF_LONG_IF8 | ctalign(3))),
    (16, ctinfo(CT_NUM, ctalign(4))),
    (16, ctinfo(CT_NUM, CTF_UNSIGNED | ctalign(4))),
    (4, ctinfo(CT_NUM, CTF_FP | ctalign(2))),
    (8, ctinfo(CT_NUM, CTF_FP | ctalign(3))),
    (8, ctinfo(CT_ARRAY, CTF_COMPLEX | ctalign(2) | CTID_FLOAT)),
    (16, ctinfo(CT_ARRAY, CTF_COMPLEX | ctalign(3) | CTID_DOUBLE)),
    (CTSIZE_PTR, ctinfo(CT_PTR, CTALIGN_PTR | CTID_VOID)),
    (CTSIZE_PTR, ctinfo(CT_PTR, CTALIGN_PTR | CTID_CVOID)),
    (CTSIZE_PTR, ctinfo(CT_PTR, CTALIGN_PTR | CTID_CCHAR)),
    (CTSIZE_PTR, ctinfo(CT_PTR, CTALIGN_PTR | CTID_UINT8)),
    (
        CTSIZE_INVALID,
        ctinfo(CT_ARRAY, CTF_CONST | ctalign(0) | CTID_CCHAR),
    ),
    (4, ctinfo(CT_ENUM, ctalign(2) | CTID_INT32)),
];

/// CTTDDEF: predefined typedefs.
const CTTDDEF: [(&str, CTypeID); 19] = [
    ("va_list", CTID_P_VOID),
    ("__builtin_va_list", CTID_P_VOID),
    ("__gnuc_va_list", CTID_P_VOID),
    ("ptrdiff_t", CTID_INT_PSZ),
    ("size_t", CTID_UINT_PSZ),
    ("wchar_t", CTID_WCHAR),
    ("int8_t", CTID_INT8),
    ("int16_t", CTID_INT16),
    ("int32_t", CTID_INT32),
    ("int64_t", CTID_INT64),
    ("int128_t", CTID_INT128),
    ("uint8_t", CTID_UINT8),
    ("uint16_t", CTID_UINT16),
    ("uint32_t", CTID_UINT32),
    ("uint64_t", CTID_UINT64),
    ("uint128_t", CTID_UINT128),
    ("intptr_t", CTID_INT_PSZ),
    ("uintptr_t", CTID_UINT_PSZ),
    ("ssize_t", CTID_INT_PSZ),
];

// -- C tokens and keywords. ------------------------------------------------

pub type CPToken = i32;

pub const CTOK_OFS: CPToken = 255;
pub const CTOK_IDENT: CPToken = 256;
pub const CTOK_STRING: CPToken = 257;
pub const CTOK_INTEGER: CPToken = 258;
pub const CTOK_EOF: CPToken = 259;
pub const CTOK_OROR: CPToken = 260;
pub const CTOK_ANDAND: CPToken = 261;
pub const CTOK_EQ: CPToken = 262;
pub const CTOK_NE: CPToken = 263;
pub const CTOK_LE: CPToken = 264;
pub const CTOK_GE: CPToken = 265;
pub const CTOK_SHL: CPToken = 266;
pub const CTOK_SHR: CPToken = 267;
pub const CTOK_DEREF: CPToken = 268;
pub const CTOK_VOID: CPToken = 269;
pub const CTOK_BOOL: CPToken = 270;
pub const CTOK_CHAR: CPToken = 271;
pub const CTOK_INT: CPToken = 272;
pub const CTOK_FP: CPToken = 273;
pub const CTOK_LONG: CPToken = 274;
pub const CTOK_LONGLONG: CPToken = 275;
pub const CTOK_SHORT: CPToken = 276;
pub const CTOK_COMPLEX: CPToken = 277;
pub const CTOK_SIGNED: CPToken = 278;
pub const CTOK_UNSIGNED: CPToken = 279;
pub const CTOK_CONST: CPToken = 280;
pub const CTOK_VOLATILE: CPToken = 281;
pub const CTOK_RESTRICT: CPToken = 282;
pub const CTOK_INLINE: CPToken = 283;
pub const CTOK_TYPEDEF: CPToken = 284;
pub const CTOK_EXTERN: CPToken = 285;
pub const CTOK_STATIC: CPToken = 286;
pub const CTOK_AUTO: CPToken = 287;
pub const CTOK_REGISTER: CPToken = 288;
pub const CTOK_EXTENSION: CPToken = 289;
pub const CTOK_ASM: CPToken = 290;
pub const CTOK_ATTRIBUTE: CPToken = 291;
pub const CTOK_DECLSPEC: CPToken = 292;
pub const CTOK_CCDECL: CPToken = 293;
pub const CTOK_PTRSZ: CPToken = 294;
pub const CTOK_STRUCT: CPToken = 295;
pub const CTOK_UNION: CPToken = 296;
pub const CTOK_ENUM: CPToken = 297;
pub const CTOK_SIZEOF: CPToken = 298;
pub const CTOK_ALIGNOF: CPToken = 299;
pub const CTOK_FIRSTDECL: CPToken = CTOK_VOID;
pub const CTOK_FIRSTSCL: CPToken = CTOK_TYPEDEF;
pub const CTOK_LASTDECLFLAG: CPToken = CTOK_REGISTER;
pub const CTOK_LASTDECL: CPToken = CTOK_ENUM;

/// Names of the CTOKDEF tokens (`CTOK_IDENT` .. `CTOK_DEREF`).
pub const CTOKNAMES: [&str; 13] = [
    "<identifier>",
    "<string>",
    "<integer>",
    "<eof>",
    "||",
    "&&",
    "==",
    "!=",
    "<=",
    ">=",
    "<<",
    ">>",
    "->",
];

/// Declaration specifier flags: `CDF_x = 1 << (CTOK_x - CTOK_FIRSTDECL)`.
pub const CDF_VOID: u32 = 1 << 0;
pub const CDF_BOOL: u32 = 1 << 1;
pub const CDF_CHAR: u32 = 1 << 2;
pub const CDF_INT: u32 = 1 << 3;
pub const CDF_FP: u32 = 1 << 4;
pub const CDF_LONG: u32 = 1 << 5;
pub const CDF_LONGLONG: u32 = 1 << 6;
pub const CDF_SHORT: u32 = 1 << 7;
pub const CDF_COMPLEX: u32 = 1 << 8;
pub const CDF_SIGNED: u32 = 1 << 9;
pub const CDF_UNSIGNED: u32 = 1 << 10;
pub const CDF_CONST: u32 = 1 << 11;
pub const CDF_VOLATILE: u32 = 1 << 12;
pub const CDF_RESTRICT: u32 = 1 << 13;
pub const CDF_INLINE: u32 = 1 << 14;
pub const CDF_TYPEDEF: u32 = 1 << 15;
pub const CDF_EXTERN: u32 = 1 << 16;
pub const CDF_STATIC: u32 = 1 << 17;
pub const CDF_AUTO: u32 = 1 << 18;
pub const CDF_REGISTER: u32 = 1 << 19;
pub const CDF_SCL: u32 = CDF_TYPEDEF | CDF_EXTERN | CDF_STATIC | CDF_AUTO | CDF_REGISTER;

/// CTKWDEF: keywords (name, size, token).
const CTKWDEF: [(&str, i32, CPToken); 58] = [
    ("void", -1, CTOK_VOID),
    ("_Bool", 0, CTOK_BOOL),
    ("bool", 1, CTOK_BOOL),
    ("char", 1, CTOK_CHAR),
    ("int", 4, CTOK_INT),
    ("__int8", 1, CTOK_INT),
    ("__int16", 2, CTOK_INT),
    ("__int32", 4, CTOK_INT),
    ("__int64", 8, CTOK_INT),
    ("__int128", 16, CTOK_INT),
    ("float", 4, CTOK_FP),
    ("double", 8, CTOK_FP),
    ("long", 0, CTOK_LONG),
    ("short", 0, CTOK_SHORT),
    ("_Complex", 0, CTOK_COMPLEX),
    ("complex", 0, CTOK_COMPLEX),
    ("__complex", 0, CTOK_COMPLEX),
    ("__complex__", 0, CTOK_COMPLEX),
    ("signed", 0, CTOK_SIGNED),
    ("__signed", 0, CTOK_SIGNED),
    ("__signed__", 0, CTOK_SIGNED),
    ("unsigned", 0, CTOK_UNSIGNED),
    ("const", 0, CTOK_CONST),
    ("__const", 0, CTOK_CONST),
    ("__const__", 0, CTOK_CONST),
    ("volatile", 0, CTOK_VOLATILE),
    ("__volatile", 0, CTOK_VOLATILE),
    ("__volatile__", 0, CTOK_VOLATILE),
    ("restrict", 0, CTOK_RESTRICT),
    ("__restrict", 0, CTOK_RESTRICT),
    ("__restrict__", 0, CTOK_RESTRICT),
    ("inline", 0, CTOK_INLINE),
    ("__inline", 0, CTOK_INLINE),
    ("__inline__", 0, CTOK_INLINE),
    ("typedef", 0, CTOK_TYPEDEF),
    ("extern", 0, CTOK_EXTERN),
    ("static", 0, CTOK_STATIC),
    ("auto", 0, CTOK_AUTO),
    ("register", 0, CTOK_REGISTER),
    ("__extension__", 0, CTOK_EXTENSION),
    ("__attribute", 0, CTOK_ATTRIBUTE),
    ("__attribute__", 0, CTOK_ATTRIBUTE),
    ("asm", 0, CTOK_ASM),
    ("__asm", 0, CTOK_ASM),
    ("__asm__", 0, CTOK_ASM),
    ("__declspec", 0, CTOK_DECLSPEC),
    ("__cdecl", CTCC_CDECL as i32, CTOK_CCDECL),
    ("__thiscall", CTCC_THISCALL as i32, CTOK_CCDECL),
    ("__fastcall", CTCC_FASTCALL as i32, CTOK_CCDECL),
    ("__stdcall", CTCC_STDCALL as i32, CTOK_CCDECL),
    ("__ptr32", 4, CTOK_PTRSZ),
    ("__ptr64", 8, CTOK_PTRSZ),
    ("struct", 0, CTOK_STRUCT),
    ("union", 0, CTOK_UNION),
    ("enum", 0, CTOK_ENUM),
    ("sizeof", 0, CTOK_SIZEOF),
    ("__alignof", 0, CTOK_ALIGNOF),
    ("__alignof__", 0, CTOK_ALIGNOF),
];

/// Number of predefined entries (types, typedefs and keywords).
pub const CTTYPEINFO_NUM: CTypeID = (CTTYDEF.len() + CTTDDEF.len() + CTKWDEF.len()) as CTypeID;

// -- C type table. ---------------------------------------------------------

/// Interned element name. `NameId::NONE` (0) means unnamed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NameId(pub u32);

impl NameId {
    pub const NONE: NameId = NameId(0);
    #[inline]
    pub fn is_some(self) -> bool {
        self.0 != 0
    }
}

/// C type table element.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CType {
    /// Type info.
    pub info: CTInfo,
    /// Type size or other info.
    pub size: CTSize,
    /// Sibling element.
    pub sib: CTypeID1,
    /// Next element in hash chain.
    pub next: CTypeID1,
    /// Element name.
    pub name: NameId,
}

/// Snapshot of the table state for rollback after a failed parse
/// (`LJ_CTYPE_SAVE`/`LJ_CTYPE_RESTORE`).
pub struct CTSaved {
    top: CTypeID,
    hash: [CTypeID1; CTHASH_SIZE],
}

#[inline]
fn hashrot(mut lo: u32, mut hi: u32) -> u32 {
    lo ^= hi;
    hi = hi.rotate_left(14);
    lo = lo.wrapping_sub(hi);
    hi = hi.rotate_left(5);
    hi ^= lo;
    hi = hi.wrapping_sub(lo.rotate_left(13));
    hi
}

#[inline]
fn ct_hashtype(info: CTInfo, size: CTSize) -> usize {
    (hashrot(info, size) & CTHASH_MASK) as usize
}

#[inline]
fn ct_hashname(name: NameId) -> usize {
    const HASH_BIAS: u32 = (-0x04c1_1db7i32) as u32;
    (hashrot(name.0, name.0.wrapping_add(HASH_BIAS)) & CTHASH_MASK) as usize
}

/// C type state.
pub struct CTState {
    tab: Vec<CType>,
    hash: [CTypeID1; CTHASH_SIZE],
    names: Vec<Box<[u8]>>,
    name_ids: HashMap<Box<[u8]>, NameId>,
    /// Callback state (slot table, machine code page, dispatch registry).
    pub cb: CallbackState,
}

impl Default for CTState {
    fn default() -> Self {
        Self::new()
    }
}

impl CTState {
    /// Initialize the C type table with the predefined types, typedefs and
    /// keywords (`lj_ctype_init`).
    pub fn new() -> CTState {
        let mut cts = CTState {
            tab: Vec::with_capacity(128),
            hash: [0; CTHASH_SIZE],
            names: vec![Box::default()],
            name_ids: HashMap::new(),
            cb: CallbackState::new(),
        };
        for (id, &(size, info)) in CTTYDEF.iter().enumerate() {
            cts.tab.push(CType {
                info,
                size,
                ..CType::default()
            });
            if !ctype_isenum(info) {
                cts.addtype(id as CTypeID);
            }
        }
        for &(name, cid) in CTTDDEF.iter() {
            cts.push_named(ctinfo(CT_TYPEDEF, cid), 0, name.as_bytes());
        }
        for &(name, sz, tok) in CTKWDEF.iter() {
            cts.push_named(ctinfo(CT_KW, tok as u32), sz as u32, name.as_bytes());
        }
        debug_assert_eq!(cts.top(), CTTYPEINFO_NUM);
        cts
    }

    fn push_named(&mut self, info: CTInfo, size: CTSize, name: &[u8]) {
        let id = self.tab.len() as CTypeID;
        let name = self.intern_name(name);
        self.tab.push(CType {
            info,
            size,
            name,
            ..CType::default()
        });
        self.addname(id);
    }

    /// Current top of the C type table (first unused id).
    #[inline]
    pub fn top(&self) -> CTypeID {
        self.tab.len() as CTypeID
    }

    /// Get C type for C type ID (`ctype_get`). Id 0 is the `CTID_NONE` entry.
    #[inline]
    pub fn get(&self, id: CTypeID) -> &CType {
        &self.tab[id as usize]
    }

    #[inline]
    pub fn get_mut(&mut self, id: CTypeID) -> &mut CType {
        &mut self.tab[id as usize]
    }

    /// Get the child type ID (`ctype_child`).
    #[inline]
    pub fn child(&self, id: CTypeID) -> CTypeID {
        ctype_cid(self.get(id).info)
    }

    /// Skip attributes (`ctype_raw`).
    #[inline]
    pub fn raw(&self, mut id: CTypeID) -> CTypeID {
        while ctype_isattrib(self.get(id).info) {
            id = self.child(id);
        }
        id
    }

    /// Raw type of the child (`ctype_rawchild`).
    #[inline]
    pub fn rawchild(&self, id: CTypeID) -> CTypeID {
        self.raw(self.child(id))
    }

    // -- Names. ------------------------------------------------------------

    /// Intern an element name.
    pub fn intern_name(&mut self, name: &[u8]) -> NameId {
        if let Some(&id) = self.name_ids.get(name) {
            return id;
        }
        let id = NameId(self.names.len() as u32);
        self.names.push(name.into());
        self.name_ids.insert(name.into(), id);
        id
    }

    /// Look up an already interned name.
    #[inline]
    pub fn find_name(&self, name: &[u8]) -> Option<NameId> {
        self.name_ids.get(name).copied()
    }

    /// The bytes of an interned name.
    #[inline]
    pub fn name_str(&self, name: NameId) -> &[u8] {
        &self.names[name.0 as usize]
    }

    /// The name of a type table element, if any.
    #[inline]
    pub fn name_of(&self, id: CTypeID) -> Option<&[u8]> {
        let n = self.get(id).name;
        n.is_some().then(|| self.name_str(n))
    }

    /// Set the name of a C type table element (`ctype_setname`).
    pub fn set_name(&mut self, id: CTypeID, name: &[u8]) {
        let n = self.intern_name(name);
        self.get_mut(id).name = n;
    }

    // -- Interning. --------------------------------------------------------

    fn check_grow(&self) -> Result<CTypeID, FfiError> {
        let id = self.top();
        if id >= CTID_MAX {
            return Err(FfiError::Msg(errmsg::TABOV.into()));
        }
        Ok(id)
    }

    /// Create new (zeroed) type element (`lj_ctype_new`).
    pub fn new_type(&mut self) -> Result<CTypeID, FfiError> {
        let id = self.check_grow()?;
        self.tab.push(CType::default());
        Ok(id)
    }

    /// Intern a type element (`lj_ctype_intern`).
    pub fn intern(&mut self, info: CTInfo, size: CTSize) -> Result<CTypeID, FfiError> {
        let h = ct_hashtype(info, size);
        let mut id = self.hash[h] as CTypeID;
        while id != 0 {
            let ct = self.get(id);
            if ct.info == info && ct.size == size {
                return Ok(id);
            }
            id = ct.next as CTypeID;
        }
        let id = self.check_grow()?;
        self.tab.push(CType {
            info,
            size,
            sib: 0,
            next: self.hash[h],
            name: NameId::NONE,
        });
        self.hash[h] = id as CTypeID1;
        Ok(id)
    }

    /// Add type element to hash table (`ctype_addtype`).
    fn addtype(&mut self, id: CTypeID) {
        let ct = self.get(id);
        let h = ct_hashtype(ct.info, ct.size);
        self.tab[id as usize].next = self.hash[h];
        self.hash[h] = id as CTypeID1;
    }

    /// Add named element to hash table (`lj_ctype_addname`).
    pub fn addname(&mut self, id: CTypeID) {
        let h = ct_hashname(self.get(id).name);
        self.tab[id as usize].next = self.hash[h];
        self.hash[h] = id as CTypeID1;
    }

    /// Get a C type by name, matching the type mask (`lj_ctype_getname`).
    /// Returns 0 if not found.
    pub fn getname(&self, name: &[u8], tmask: u32) -> CTypeID {
        match self.find_name(name) {
            Some(n) => self.getname_id(n, tmask),
            None => 0,
        }
    }

    /// Like [`CTState::getname`] for an interned name.
    pub fn getname_id(&self, name: NameId, tmask: u32) -> CTypeID {
        let mut id = self.hash[ct_hashname(name)] as CTypeID;
        while id != 0 {
            let ct = self.get(id);
            if ct.name == name && ((tmask >> ctype_type(ct.info)) & 1) != 0 {
                return id;
            }
            id = ct.next as CTypeID;
        }
        0
    }

    /// Get a struct/union/enum/function field by name (`lj_ctype_getfieldq`).
    /// Returns the field id, its offset and the merged qualifiers of the
    /// transparent sub-types it was found in.
    pub fn getfieldq(&self, id: CTypeID, name: &[u8]) -> Option<(CTypeID, CTSize, CTInfo)> {
        let n = self.find_name(name)?;
        self.getfieldq_id(id, n)
    }

    fn getfieldq_id(&self, mut id: CTypeID, name: NameId) -> Option<(CTypeID, CTSize, CTInfo)> {
        while self.get(id).sib != 0 {
            id = self.get(id).sib as CTypeID;
            let ct = self.get(id);
            if ct.name == name {
                return Some((id, ct.size, 0));
            }
            if ctype_isxattrib(ct.info, CTA_SUBTYPE) {
                let mut cct = self.child(id);
                let mut q = 0;
                while ctype_isattrib(self.get(cct).info) {
                    if ctype_attrib(self.get(cct).info) == CTA_QUAL {
                        q |= self.get(cct).size;
                    }
                    cct = self.child(cct);
                }
                if let Some((fid, ofs, qual)) = self.getfieldq_id(cct, name) {
                    return Some((fid, ofs + ct.size, qual | q));
                }
            }
        }
        None
    }

    /// Get a field by name (`lj_ctype_getfield`): field id and offset.
    pub fn getfield(&self, id: CTypeID, name: &[u8]) -> Option<(CTypeID, CTSize)> {
        self.getfieldq(id, name).map(|(f, ofs, _)| (f, ofs))
    }

    // -- C type information. -----------------------------------------------

    /// Follow references and attributes (`lj_ctype_rawref`).
    pub fn rawref(&self, mut id: CTypeID) -> CTypeID {
        loop {
            let info = self.get(id).info;
            if !(ctype_isattrib(info) || ctype_isref(info)) {
                return id;
            }
            id = ctype_cid(info);
        }
    }

    /// Size of a C type. Does NOT support VLA/VLS (`lj_ctype_size`).
    pub fn size(&self, id: CTypeID) -> CTSize {
        let ct = self.get(self.raw(id));
        if ctype_hassize(ct.info) {
            ct.size
        } else {
            CTSIZE_INVALID
        }
    }

    /// Size of a variable-length type with `nelem` elements (`lj_ctype_vlsize`).
    pub fn vlsize(&self, mut id: CTypeID, nelem: CTSize) -> CTSize {
        let mut xsz: u64 = 0;
        let ct = self.get(id);
        if ctype_isstruct(ct.info) {
            let mut arrid = 0;
            let mut fid = ct.sib as CTypeID;
            xsz = ct.size as u64;
            while fid != 0 {
                let ctf = self.get(fid);
                if ctype_type(ctf.info) == CT_FIELD {
                    arrid = ctype_cid(ctf.info);
                }
                fid = ctf.sib as CTypeID;
            }
            id = self.raw(arrid);
        }
        debug_assert!(ctype_isvlarray(self.get(id).info), "VLA expected");
        let elem = self.get(self.rawchild(id));
        xsz += elem.size as u64 * nelem as u64;
        if xsz < 0x8000_0000 {
            xsz as CTSize
        } else {
            CTSIZE_INVALID
        }
    }

    /// Type, qualifiers, size and alignment for a C type ID (`lj_ctype_info`).
    pub fn info(&self, mut id: CTypeID) -> (CTInfo, CTSize) {
        let mut qual: CTInfo = 0;
        loop {
            let ct = self.get(id);
            let info = ct.info;
            if ctype_isenum(info) {
                // Follow child. Need to look at its attributes, too.
            } else if ctype_isattrib(info) {
                if ctype_isxattrib(info, CTA_QUAL) {
                    qual |= ct.size;
                } else if ctype_isxattrib(info, CTA_ALIGN) && (qual & CTFP_ALIGNED) == 0 {
                    qual |= CTFP_ALIGNED + ctalign(ct.size);
                }
            } else {
                if (qual & CTFP_ALIGNED) == 0 {
                    qual |= info & CTF_ALIGN;
                }
                qual |= info & !(CTF_ALIGN | CTMASK_CID);
                debug_assert!(
                    ctype_hassize(info) || ctype_isfunc(info),
                    "ctype without size"
                );
                let sz = if ctype_isfunc(info) {
                    CTSIZE_INVALID
                } else {
                    ct.size
                };
                return (qual, sz);
            }
            id = ctype_cid(info);
        }
    }

    /// Ditto, but follow a reference (`lj_ctype_info_raw`).
    pub fn info_raw(&self, mut id: CTypeID) -> (CTInfo, CTSize) {
        let ct = self.get(id);
        if ctype_isref(ct.info) {
            id = ctype_cid(ct.info);
        }
        self.info(id)
    }

    /// For the metatable lookup of `lj_ctype_meta`: follow attributes and
    /// references. Returns `None` if the type is a pointer to a function
    /// (whose metatable is the shared one stored under the empty string key),
    /// otherwise the id whose negation keys the per-type metatable.
    pub fn meta_key(&self, mut id: CTypeID) -> Option<CTypeID> {
        loop {
            let info = self.get(id).info;
            if !(ctype_isattrib(info) || ctype_isref(info)) {
                break;
            }
            id = ctype_cid(info);
        }
        let info = self.get(id).info;
        if ctype_isptr(info) && ctype_isfunc(self.get(ctype_cid(info)).info) {
            None
        } else {
            Some(id)
        }
    }

    // -- Rollback. ---------------------------------------------------------

    /// `LJ_CTYPE_SAVE`.
    pub fn save(&self) -> CTSaved {
        CTSaved {
            top: self.top(),
            hash: self.hash,
        }
    }

    /// `LJ_CTYPE_RESTORE`.
    pub fn restore(&mut self, saved: &CTSaved) {
        self.tab.truncate(saved.top as usize);
        self.hash = saved.hash;
    }

    // -- Representation. ---------------------------------------------------

    /// Printable representation of a C type, optionally with a declared name
    /// (`lj_ctype_repr`), e.g. `struct foo *` or `int (*)(int)`.
    pub fn repr(&self, id: CTypeID, name: Option<&[u8]>) -> Vec<u8> {
        let mut ctr = CTRepr::new(self);
        if let Some(name) = name {
            ctr.prepstr(name);
        }
        ctr.repr(id);
        if !ctr.ok {
            return b"?".to_vec();
        }
        ctr.buf[ctr.pb..ctr.pe].to_vec()
    }
}

/// Fixed max. length of a C type representation.
const CTREPR_MAX: usize = 512;

struct CTRepr<'a> {
    pb: usize,
    pe: usize,
    cts: &'a CTState,
    needsp: bool,
    ok: bool,
    buf: [u8; CTREPR_MAX],
}

impl<'a> CTRepr<'a> {
    fn new(cts: &'a CTState) -> Self {
        CTRepr {
            pb: CTREPR_MAX / 2,
            pe: CTREPR_MAX / 2,
            cts,
            needsp: false,
            ok: true,
            buf: [0; CTREPR_MAX],
        }
    }

    /// Prepend string.
    fn prepstr(&mut self, s: &[u8]) {
        let mut p = self.pb;
        if s.len() + 1 > p {
            self.ok = false;
            return;
        }
        if self.needsp {
            p -= 1;
            self.buf[p] = b' ';
        }
        self.needsp = true;
        p -= s.len();
        self.buf[p..p + s.len()].copy_from_slice(s);
        self.pb = p;
    }

    /// Prepend char.
    fn prepc(&mut self, c: u8) {
        if self.pb == 0 {
            self.ok = false;
            return;
        }
        self.pb -= 1;
        self.buf[self.pb] = c;
    }

    /// Prepend number.
    fn prepnum(&mut self, mut n: u32) {
        let mut p = self.pb;
        if 10 + 1 > p {
            self.ok = false;
            return;
        }
        loop {
            p -= 1;
            self.buf[p] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        self.pb = p;
        self.needsp = false;
    }

    /// Append char.
    fn appc(&mut self, c: u8) {
        if self.pe >= CTREPR_MAX {
            self.ok = false;
            return;
        }
        self.buf[self.pe] = c;
        self.pe += 1;
    }

    /// Append number.
    fn appnum(&mut self, n: u32) {
        if self.pe > CTREPR_MAX - 10 {
            self.ok = false;
            return;
        }
        let s = n.to_string();
        self.buf[self.pe..self.pe + s.len()].copy_from_slice(s.as_bytes());
        self.pe += s.len();
    }

    /// Prepend qualifiers.
    fn prepqual(&mut self, info: CTInfo) {
        if info & CTF_VOLATILE != 0 {
            self.prepstr(b"volatile");
        }
        if info & CTF_CONST != 0 {
            self.prepstr(b"const");
        }
    }

    /// Prepend named type.
    fn preptype(&mut self, id: CTypeID, qual: CTInfo, t: &[u8]) {
        if let Some(name) = self.cts.name_of(id) {
            self.prepstr(name);
        } else {
            if self.needsp {
                self.prepc(b' ');
            }
            self.prepnum(id);
            self.needsp = true;
        }
        self.prepstr(t);
        self.prepqual(qual);
    }

    fn repr(&mut self, mut id: CTypeID) {
        let cts = self.cts;
        let mut qual: CTInfo = 0;
        let mut ptrto = false;
        loop {
            let ct = cts.get(id);
            let info = ct.info;
            let size = ct.size;
            match ctype_type(info) {
                CT_NUM => {
                    if info & CTF_BOOL != 0 {
                        self.prepstr(b"bool");
                    } else if info & CTF_FP != 0 {
                        if size == 8 {
                            self.prepstr(b"double");
                        } else if size == 4 {
                            self.prepstr(b"float");
                        } else {
                            self.prepstr(b"long double");
                        }
                    } else if size == 1 {
                        if ((info ^ CTF_UCHAR) & CTF_UNSIGNED) == 0 {
                            self.prepstr(b"char");
                        } else if CTF_UCHAR != 0 {
                            self.prepstr(b"signed char");
                        } else {
                            self.prepstr(b"unsigned char");
                        }
                    } else if size < 8 {
                        if size == 4 {
                            self.prepstr(b"int");
                        } else {
                            self.prepstr(b"short");
                        }
                        if info & CTF_UNSIGNED != 0 {
                            self.prepstr(b"unsigned");
                        }
                    } else {
                        self.prepstr(b"_t");
                        self.prepnum(size.wrapping_mul(8));
                        self.prepstr(b"int");
                        if info & CTF_UNSIGNED != 0 {
                            self.prepc(b'u');
                        }
                    }
                    self.prepqual(qual | info);
                    return;
                }
                CT_VOID => {
                    self.prepstr(b"void");
                    self.prepqual(qual | info);
                    return;
                }
                CT_STRUCT => {
                    let t: &[u8] = if info & CTF_UNION != 0 {
                        b"union"
                    } else {
                        b"struct"
                    };
                    self.preptype(id, qual, t);
                    return;
                }
                CT_ENUM => {
                    if id == CTID_CTYPEID {
                        self.prepstr(b"ctype");
                        return;
                    }
                    self.preptype(id, qual, b"enum");
                    return;
                }
                CT_ATTRIB => {
                    if ctype_attrib(info) == CTA_QUAL {
                        qual |= size;
                    }
                }
                CT_PTR => {
                    if info & CTF_REF != 0 {
                        self.prepc(b'&');
                    } else {
                        self.prepqual(qual | info);
                        if size == 4 {
                            self.prepstr(b"__ptr32");
                        }
                        self.prepc(b'*');
                    }
                    qual = 0;
                    ptrto = true;
                    self.needsp = true;
                }
                CT_ARRAY => {
                    if ctype_isrefarray(info) {
                        self.needsp = true;
                        if ptrto {
                            ptrto = false;
                            self.prepc(b'(');
                            self.appc(b')');
                        }
                        self.appc(b'[');
                        if size != CTSIZE_INVALID {
                            let csize = cts.get(ctype_cid(info)).size;
                            self.appnum(size.checked_div(csize).unwrap_or(0));
                        } else if info & CTF_VLA != 0 {
                            self.appc(b'?');
                        }
                        self.appc(b']');
                    } else if info & CTF_COMPLEX != 0 {
                        if size == 8 {
                            self.prepstr(b"float");
                        }
                        self.prepstr(b"complex");
                        return;
                    } else {
                        self.prepstr(b")))");
                        self.prepnum(size);
                        self.prepstr(b"__attribute__((vector_size(");
                    }
                }
                CT_FUNC => {
                    self.needsp = true;
                    if ptrto {
                        ptrto = false;
                        self.prepc(b'(');
                        self.appc(b')');
                    }
                    self.appc(b'(');
                    self.appc(b')');
                }
                _ => {
                    debug_assert!(false, "bad ctype {info:08x}");
                }
            }
            id = ctype_cid(info);
        }
    }
}

/// Convert int64_t/uint64_t to string with `LL` or `ULL` suffix
/// (`lj_ctype_repr_int64`).
pub fn repr_int64(n: u64, isunsigned: bool) -> Vec<u8> {
    let mut s = if isunsigned {
        n.to_string()
    } else {
        (n as i64).to_string()
    };
    if isunsigned {
        s.push('U');
    }
    s.push_str("LL");
    s.into_bytes()
}

/// Convert complex to string with `i` or `I` suffix (`lj_ctype_repr_complex`).
/// `sp` holds the raw complex value of `size` bytes (8: `complex float`,
/// 16: `complex double`).
pub fn repr_complex(sp: &[u8], size: CTSize) -> Vec<u8> {
    let (re, im) = if size == 16 {
        (
            f64::from_ne_bytes(sp[0..8].try_into().unwrap()),
            f64::from_ne_bytes(sp[8..16].try_into().unwrap()),
        )
    } else {
        (
            f32::from_ne_bytes(sp[0..4].try_into().unwrap()) as f64,
            f32::from_ne_bytes(sp[4..8].try_into().unwrap()) as f64,
        )
    };
    let mut sb = Vec::with_capacity(48);
    fmt_g14(&mut sb, re);
    if (im.to_bits() >> 63) == 0 || im.is_nan() {
        sb.push(b'+');
    }
    fmt_g14(&mut sb, im);
    let last = *sb.last().unwrap();
    sb.push(if last >= b'a' { b'I' } else { b'i' });
    sb
}

/// Append a number formatted like `lj_strfmt_putfnum(sb, STRFMT_G14, n)`:
/// `%.14g`, with `inf`/`-inf`/`nan` for non-finite values.
pub fn fmt_g14(sb: &mut Vec<u8>, n: f64) {
    if n.is_nan() {
        sb.extend_from_slice(b"nan");
    } else if n.is_infinite() {
        sb.extend_from_slice(if n < 0.0 { b"-inf" } else { b"inf" });
    } else {
        let mut buf = [0u8; 64];
        // SAFETY: the buffer is large enough for any %.14g output of a finite
        // double (at most 21 chars) and snprintf NUL-terminates within bounds.
        let len =
            unsafe { libc::snprintf(buf.as_mut_ptr().cast(), buf.len(), c"%.14g".as_ptr(), n) };
        sb.extend_from_slice(&buf[..len as usize]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predefined_ids() {
        let cts = CTState::new();
        assert_eq!(cts.top(), 102);
        assert_eq!(cts.getname(b"size_t", 1 << CT_TYPEDEF), 29);
        assert_eq!(cts.child(29), CTID_UINT64);
        let kw = cts.getname(b"__alignof__", 1 << CT_KW);
        assert_eq!(ctype_cid(cts.get(kw).info), CTOK_ALIGNOF as u32);
        let v = cts.getname(b"void", 1 << CT_KW);
        assert_eq!(cts.get(v).size, CTSIZE_INVALID);
        assert_eq!(cts.getname(b"size_t", 1 << CT_KW), 0);
    }

    #[test]
    fn intern_dedups() {
        let mut cts = CTState::new();
        let a = cts
            .intern(ctinfo(CT_PTR, CTALIGN_PTR | CTID_VOID), CTSIZE_PTR)
            .unwrap();
        assert_eq!(a, CTID_P_VOID);
        let b = cts
            .intern(ctinfo(CT_PTR, CTALIGN_PTR | CTID_INT32), CTSIZE_PTR)
            .unwrap();
        assert_eq!(b, cts.top() - 1);
        assert_eq!(
            cts.intern(ctinfo(CT_PTR, CTALIGN_PTR | CTID_INT32), CTSIZE_PTR)
                .unwrap(),
            b
        );
    }

    #[test]
    fn repr_predefined() {
        let cts = CTState::new();
        let r = |id| String::from_utf8(cts.repr(id, None)).unwrap();
        assert_eq!(r(CTID_INT64), "int64_t");
        assert_eq!(r(CTID_UINT64), "uint64_t");
        assert_eq!(r(CTID_UINT128), "uint128_t");
        assert_eq!(r(CTID_CCHAR), "const char");
        assert_eq!(r(CTID_P_CCHAR), "const char *");
        assert_eq!(r(CTID_A_CCHAR), "const char []");
        assert_eq!(r(CTID_COMPLEX_FLOAT), "complex float");
        assert_eq!(r(CTID_COMPLEX_DOUBLE), "complex");
        assert_eq!(r(CTID_P_CVOID), "const void *");
        assert_eq!(r(CTID_CTYPEID), "ctype");
        assert_eq!(r(CTID_UINT8), "unsigned char");
        assert_eq!(r(CTID_INT8), "char");
        assert_eq!(r(CTID_UINT16), "unsigned short");
        assert_eq!(cts.repr(CTID_INT32, Some(b"x")), b"int x");
    }

    #[test]
    fn repr_numbers() {
        // Values from the reference: tostring(-1LL), tostring(-1ULL), ...
        assert_eq!(repr_int64(0, false), b"0LL");
        assert_eq!(repr_int64(u64::MAX, false), b"-1LL");
        assert_eq!(repr_int64(u64::MAX, true), b"18446744073709551615ULL");
        assert_eq!(repr_int64(1u64 << 63, false), b"-9223372036854775808LL");
        let c = |re: f64, im: f64| {
            let mut b = re.to_ne_bytes().to_vec();
            b.extend_from_slice(&im.to_ne_bytes());
            String::from_utf8(repr_complex(&b, 16)).unwrap()
        };
        assert_eq!(c(f64::NAN, f64::INFINITY), "nan+infI");
        assert_eq!(c(1.0, -2.0), "1-2i");
        assert_eq!(c(1e300, -1e-300), "1e+300-1e-300i");
        assert_eq!(c(-0.0, -0.0), "-0-0i");
        assert_eq!(c(1.0, -f64::NAN), "1+nanI");
        assert_eq!(c(0.0, 12.5), "0+12.5i");
        assert_eq!(c(0.0, f64::NEG_INFINITY), "0-infI");
        let mut b = 1.5f32.to_ne_bytes().to_vec();
        b.extend_from_slice(&f32::NAN.to_ne_bytes());
        assert_eq!(repr_complex(&b, 8), b"1.5+nanI");
        let mut s = Vec::new();
        fmt_g14(&mut s, 123456789012345678.0);
        assert_eq!(s, b"1.2345678901235e+17");
    }
}
