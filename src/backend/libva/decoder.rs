use libva::{Display, VAConfigAttrib, VAConfigAttribType, VAEntrypoint};

struct LibvaDecoder {
    display: std::rc::Rc<libva::Display>,
    config: libva::Config,
    context: std::rc::Rc<libva::Context>,
    free_surfaces: std::collections::VecDeque<libva::Surface<()>>,
    held_surfaces: Vec<libva::Surface<()>>, // references/cache-owned
}

const H264_PROFILES: &[libva::VAProfile::Type] = &[
    libva::VAProfile::VAProfileH264Baseline,
    libva::VAProfile::VAProfileH264Main,
    libva::VAProfile::VAProfileH264High,
];

impl LibvaDecoder {
    fn new() -> Option<Self> {
        let display = Display::open()?;
        let profiles = display.query_config_profiles().ok()?;
        let preferred = profiles.iter().find(|p| H264_PROFILES.contains(p))?;
        let entrypoints = display.query_config_entrypoints(*preferred).ok()?;
        if !entrypoints.contains(&VAEntrypoint::VAEntrypointVLD) {
            return None;
        }
        let entrypoint = VAEntrypoint::VAEntrypointVLD;
        let mut attrs = vec![VAConfigAttrib {
            type_: VAConfigAttribType::VAConfigAttribRTFormat,
            value: 0,
        }];
        display
            .get_config_attributes(*preferred, entrypoint, &mut attrs)
            .ok()?;
        let rt_attr = attrs[0].value;
        if rt_attr == libva::VA_ATTRIB_NOT_SUPPORTED
            || (rt_attr & libva::VA_RT_FORMAT_YUV420) == 0
        {
            return None;
        }
        let config = display.create_config(attrs, *preferred, entrypoint).ok()?;
        let context = display.create_context(&config, coded_width, coded_height, surfaces, progressive).ok()?;
        Some(Self { display, config, context })
    }
}
