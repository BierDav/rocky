// Declarations from rust-lang/libc 0.2.190 (MIT OR Apache-2.0), available in XNU on iOS.
#![allow(non_camel_case_types)]
use crate::*;
pub const RTF_UP: c_int = 0x1;
pub const RTF_GATEWAY: c_int = 0x2;
pub const RTF_HOST: c_int = 0x4;
pub const RTF_REJECT: c_int = 0x8;
pub const RTF_DYNAMIC: c_int = 0x10;
pub const RTF_MODIFIED: c_int = 0x20;
pub const RTF_DONE: c_int = 0x40;
pub const RTF_STATIC: c_int = 0x800;
pub const RTF_BLACKHOLE: c_int = 0x1000;
pub const RTF_PROTO2: c_int = 0x4000;
pub const RTF_PROTO1: c_int = 0x8000;
pub const RTM_ADD: c_int = 0x1;
pub const RTM_DELETE: c_int = 0x2;
pub const RTM_CHANGE: c_int = 0x3;
pub const RTM_GET: c_int = 0x4;
pub const RTM_LOSING: c_int = 0x5;
pub const RTM_REDIRECT: c_int = 0x6;
pub const RTM_MISS: c_int = 0x7;
pub const RTA_DST: c_int = 0x1;
pub const RTA_GATEWAY: c_int = 0x2;
pub const RTA_NETMASK: c_int = 0x4;
pub const RTA_GENMASK: c_int = 0x8;
pub const RTA_IFP: c_int = 0x10;
pub const RTA_IFA: c_int = 0x20;
pub const RTA_AUTHOR: c_int = 0x40;
pub const RTA_BRD: c_int = 0x80;
pub const RTAX_DST: c_int = 0;
pub const RTAX_GATEWAY: c_int = 1;
pub const RTAX_NETMASK: c_int = 2;
pub const RTAX_GENMASK: c_int = 3;
pub const RTAX_IFP: c_int = 4;
pub const RTAX_IFA: c_int = 5;
pub const RTAX_AUTHOR: c_int = 6;
pub const RTAX_BRD: c_int = 7;
pub const RTF_DELCLONE: c_int = 0x80;
pub const RTF_CLONING: c_int = 0x100;
pub const RTF_XRESOLVE: c_int = 0x200;
pub const RTF_LLINFO: c_int = 0x400;
pub const RTF_NOIFREF: c_int = 0x2000;
pub const RTF_PRCLONING: c_int = 0x10000;
pub const RTF_WASCLONED: c_int = 0x20000;
pub const RTF_PROTO3: c_int = 0x40000;
pub const RTF_PINNED: c_int = 0x100000;
pub const RTF_LOCAL: c_int = 0x200000;
pub const RTF_BROADCAST: c_int = 0x400000;
pub const RTF_MULTICAST: c_int = 0x800000;
pub const RTF_IFSCOPE: c_int = 0x1000000;
pub const RTF_CONDEMNED: c_int = 0x2000000;
pub const RTF_IFREF: c_int = 0x4000000;
pub const RTF_PROXY: c_int = 0x8000000;
pub const RTF_ROUTER: c_int = 0x10000000;
pub const RTF_DEAD: c_int = 0x20000000;
pub const RTF_GLOBAL: c_int = 0x40000000;
pub const RTM_VERSION: c_int = 5;
pub const RTM_LOCK: c_int = 0x8;
pub const RTM_OLDADD: c_int = 0x9;
pub const RTM_OLDDEL: c_int = 0xa;
pub const RTM_RESOLVE: c_int = 0xb;
pub const RTM_NEWADDR: c_int = 0xc;
pub const RTM_DELADDR: c_int = 0xd;
pub const RTM_IFINFO: c_int = 0xe;
pub const RTM_NEWMADDR: c_int = 0xf;
pub const RTM_DELMADDR: c_int = 0x10;
pub const RTM_IFINFO2: c_int = 0x12;
pub const RTM_NEWMADDR2: c_int = 0x13;
pub const RTM_GET2: c_int = 0x14;
pub const RTV_MTU: c_int = 0x1;
pub const RTV_HOPCOUNT: c_int = 0x2;
pub const RTV_EXPIRE: c_int = 0x4;
pub const RTV_RPIPE: c_int = 0x8;
pub const RTV_SPIPE: c_int = 0x10;
pub const RTV_SSTHRESH: c_int = 0x20;
pub const RTV_RTT: c_int = 0x40;
pub const RTV_RTTVAR: c_int = 0x80;
pub const RTAX_MAX: c_int = 8;
#[repr(C)]
#[derive(Copy, Clone)]
pub struct in6_addrlifetime {
        pub ia6t_expire: time_t,
        pub ia6t_preferred: time_t,
        pub ia6t_vltime: u32,
        pub ia6t_pltime: u32,
    }

#[repr(C)]
#[derive(Copy, Clone)]
pub struct in6_ifstat {
        pub ifs6_in_receive: crate::u_quad_t,
        pub ifs6_in_hdrerr: crate::u_quad_t,
        pub ifs6_in_toobig: crate::u_quad_t,
        pub ifs6_in_noroute: crate::u_quad_t,
        pub ifs6_in_addrerr: crate::u_quad_t,
        pub ifs6_in_protounknown: crate::u_quad_t,
        pub ifs6_in_truncated: crate::u_quad_t,
        pub ifs6_in_discard: crate::u_quad_t,
        pub ifs6_in_deliver: crate::u_quad_t,
        pub ifs6_out_forward: crate::u_quad_t,
        pub ifs6_out_request: crate::u_quad_t,
        pub ifs6_out_discard: crate::u_quad_t,
        pub ifs6_out_fragok: crate::u_quad_t,
        pub ifs6_out_fragfail: crate::u_quad_t,
        pub ifs6_out_fragcreat: crate::u_quad_t,
        pub ifs6_reass_reqd: crate::u_quad_t,
        pub ifs6_reass_ok: crate::u_quad_t,
        pub ifs6_atmfrag_rcvd: crate::u_quad_t,
        pub ifs6_reass_fail: crate::u_quad_t,
        pub ifs6_in_mcast: crate::u_quad_t,
        pub ifs6_out_mcast: crate::u_quad_t,
        pub ifs6_cantfoward_icmp6: crate::u_quad_t,
        pub ifs6_addr_expiry_cnt: crate::u_quad_t,
        pub ifs6_pfx_expiry_cnt: crate::u_quad_t,
        pub ifs6_defrtr_expiry_cnt: crate::u_quad_t,
    }

#[repr(C)]
#[derive(Copy, Clone)]
pub struct icmp6_ifstat {
        pub ifs6_in_msg: crate::u_quad_t,
        pub ifs6_in_error: crate::u_quad_t,
        pub ifs6_in_dstunreach: crate::u_quad_t,
        pub ifs6_in_adminprohib: crate::u_quad_t,
        pub ifs6_in_timeexceed: crate::u_quad_t,
        pub ifs6_in_paramprob: crate::u_quad_t,
        pub ifs6_in_pkttoobig: crate::u_quad_t,
        pub ifs6_in_echo: crate::u_quad_t,
        pub ifs6_in_echoreply: crate::u_quad_t,
        pub ifs6_in_routersolicit: crate::u_quad_t,
        pub ifs6_in_routeradvert: crate::u_quad_t,
        pub ifs6_in_neighborsolicit: crate::u_quad_t,
        pub ifs6_in_neighboradvert: crate::u_quad_t,
        pub ifs6_in_redirect: crate::u_quad_t,
        pub ifs6_in_mldquery: crate::u_quad_t,
        pub ifs6_in_mldreport: crate::u_quad_t,
        pub ifs6_in_mlddone: crate::u_quad_t,
        pub ifs6_out_msg: crate::u_quad_t,
        pub ifs6_out_error: crate::u_quad_t,
        pub ifs6_out_dstunreach: crate::u_quad_t,
        pub ifs6_out_adminprohib: crate::u_quad_t,
        pub ifs6_out_timeexceed: crate::u_quad_t,
        pub ifs6_out_paramprob: crate::u_quad_t,
        pub ifs6_out_pkttoobig: crate::u_quad_t,
        pub ifs6_out_echo: crate::u_quad_t,
        pub ifs6_out_echoreply: crate::u_quad_t,
        pub ifs6_out_routersolicit: crate::u_quad_t,
        pub ifs6_out_routeradvert: crate::u_quad_t,
        pub ifs6_out_neighborsolicit: crate::u_quad_t,
        pub ifs6_out_neighboradvert: crate::u_quad_t,
        pub ifs6_out_redirect: crate::u_quad_t,
        pub ifs6_out_mldquery: crate::u_quad_t,
        pub ifs6_out_mldreport: crate::u_quad_t,
        pub ifs6_out_mlddone: crate::u_quad_t,
    }

#[repr(C)]
#[derive(Copy, Clone)]
pub struct in6_ifreq {
        pub ifr_name: [c_char; crate::IFNAMSIZ],
        pub ifr_ifru: __c_anonymous_ifr_ifru6,
    }
#[repr(C)]
#[derive(Copy, Clone)]
pub union __c_anonymous_ifr_ifru6 {
                pub ifru_addr: crate::sockaddr_in6,
                pub ifru_dstaddr: crate::sockaddr_in6,
                pub ifru_flags: c_int,
                pub ifru_flags6: c_int,
                pub ifru_metrics: c_int,
                pub ifru_intval: c_int,
                pub ifru_data: *mut c_char,
                pub ifru_lifetime: in6_addrlifetime,
                pub ifru_stat: in6_ifstat,
                pub ifru_icmp6stat: icmp6_ifstat,
                pub ifru_scope_id: [u32; SCOPE6_ID_MAX],
            }

const _: () = {
    assert!(core::mem::size_of::<in6_ifreq>() == 288);
    assert!(core::mem::align_of::<in6_ifreq>() == 8);
    assert!(core::mem::offset_of!(in6_ifreq, ifr_ifru) == 16);
};
