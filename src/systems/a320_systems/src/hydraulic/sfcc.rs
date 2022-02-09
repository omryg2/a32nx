use systems::shared::{FeedbackPositionPickoffUnit, LgciuWeightOnWheels};
use systems::navigation::adirs;
use systems::landing_gear::{LandingGearControlInterfaceUnit,};
use systems::simulation::{
    InitContext, Read, SimulationElement, SimulationElementVisitor, SimulatorReader,
    SimulatorWriter, UpdateContext, VariableIdentifier, Write,
};
use systems::navigation::adirs::{AirDataInertialReferenceSystem, AirDataInertialReferenceUnit};

use crate::systems::shared::arinc429::{Arinc429Word, SignStatus};

use std::panic;
use uom::si::{angle::degree, f64::*, velocity::knot};

#[derive(Debug, Copy, Clone, PartialEq)]
enum FlapsConf {
    Conf0,
    Conf1,
    Conf1F,
    Conf2,
    Conf3,
    ConfFull,
}

impl From<u8> for FlapsConf {
    fn from(value: u8) -> Self {
        match value {
            0 => FlapsConf::Conf0,
            1 => FlapsConf::Conf1,
            2 => FlapsConf::Conf1F,
            3 => FlapsConf::Conf2,
            4 => FlapsConf::Conf3,
            5 => FlapsConf::ConfFull,
            i => panic!("Cannot convert from {} to FlapsConf.", i),
        }
    }
}

/// A struct to read the handle position
struct FlapsHandle {
    handle_position_id: VariableIdentifier,
    position: u8,
    previous_position: u8,
}

fn demanded_flaps_angle_from_conf(flap_conf: FlapsConf) -> Angle {
    match flap_conf {
        FlapsConf::Conf0 => Angle::new::<degree>(0.),
        FlapsConf::Conf1 => Angle::new::<degree>(0.),
        FlapsConf::Conf1F => Angle::new::<degree>(10.),
        FlapsConf::Conf2 => Angle::new::<degree>(15.),
        FlapsConf::Conf3 => Angle::new::<degree>(20.),
        FlapsConf::ConfFull => Angle::new::<degree>(40.),
    }
}

fn demanded_slats_angle_from_conf(flap_conf: FlapsConf) -> Angle {
    match flap_conf {
        FlapsConf::Conf0 => Angle::new::<degree>(0.),
        FlapsConf::Conf1 => Angle::new::<degree>(18.),
        FlapsConf::Conf1F => Angle::new::<degree>(18.),
        FlapsConf::Conf2 => Angle::new::<degree>(22.),
        FlapsConf::Conf3 => Angle::new::<degree>(22.),
        FlapsConf::ConfFull => Angle::new::<degree>(27.),
    }
}


impl FlapsHandle {
    fn new(context: &mut InitContext) -> Self {
        Self {
            handle_position_id: context.get_identifier("FLAPS_HANDLE_INDEX".to_owned()),
            position: 0,
            previous_position: 0,
        }
    }

    fn position(&self) -> u8 {
        self.position
    }

    fn previous_position(&self) -> u8 {
        self.previous_position
    }
}

impl SimulationElement for FlapsHandle {
    fn read(&mut self, reader: &mut SimulatorReader) {
        self.previous_position = self.position;
        self.position = reader.read(&self.handle_position_id);
    }
}


trait SfccChannel {
    fn receive_signal(&mut self, feedback: &impl FeedbackPositionPickoffUnit);
    fn send_signal(&self) -> bool;
    fn generate_configuration(&self, context: &UpdateContext, flaps_handle: &FlapsHandle, adiru: &AirDataInertialReferenceUnit) -> FlapsConf;
}

struct FlapsChannel {
    feedback_angle: Angle,
    demanded_angle: Angle,
    calculated_conf: FlapsConf,
}

impl FlapsChannel {
    const HANDLE_ONE_CONF_AIRSPEED_THRESHOLD_KNOTS: f64 = 100.;
    const CONF1F_TO_CONF1_AIRSPEED_THRESHOLD_KNOTS: f64 = 210.;
    const EQUAL_ANGLE_DELTA_DEGREE: f64 = 0.01;

    pub fn new() -> Self {
        Self {
            feedback_angle: Angle::new::<degree>(0.),
            demanded_angle: Angle::new::<degree>(0.),
            calculated_conf: FlapsConf::Conf0,
        }
    }

    pub fn update(&mut self, context: &UpdateContext, flaps_handle: &FlapsHandle, feedback: &impl FeedbackPositionPickoffUnit, adiru: &AirDataInertialReferenceUnit) {

        self.receive_signal(feedback);
        self.calculated_conf = self.generate_configuration(context,flaps_handle,adiru);
        self.demanded_angle = demanded_flaps_angle_from_conf(self.calculated_conf);
    }

    fn feedback_equals_demanded(&self) -> bool {
        (self.demanded_angle - self.feedback_angle).get::<degree>().abs() < Self::EQUAL_ANGLE_DELTA_DEGREE
    }
}

impl SfccChannel for FlapsChannel {

    fn receive_signal(&mut self, feedback: &impl FeedbackPositionPickoffUnit) {
        self.feedback_angle = feedback.angle();
    }
    fn send_signal(&self) -> bool {
        !self.feedback_equals_demanded()
     }

    fn generate_configuration(
        &self,
        context: &UpdateContext,
        flaps_handle: &FlapsHandle,
        adiru: &AirDataInertialReferenceUnit,
    ) -> FlapsConf {

        let computed_airspeed: Velocity =
            match adiru.computed_airspeed().ssm() {
                SignStatus::NormalOperation => adiru.computed_airspeed().value(),
                _ => context.indicated_airspeed(),
            };

        match (flaps_handle.previous_position(), flaps_handle.position()) {
            (0, 1)
                if computed_airspeed.get::<knot>()
                    <= Self::HANDLE_ONE_CONF_AIRSPEED_THRESHOLD_KNOTS =>
            {
                FlapsConf::Conf1F
            }
            (0, 1) => FlapsConf::Conf1,
            (1, 1)
                if computed_airspeed.get::<knot>()
                    > Self::CONF1F_TO_CONF1_AIRSPEED_THRESHOLD_KNOTS =>
            {
                FlapsConf::Conf1
            }
            (1, 1) => self.calculated_conf,
            (_, 1)
                if computed_airspeed.get::<knot>()
                    <= Self::CONF1F_TO_CONF1_AIRSPEED_THRESHOLD_KNOTS =>
            {
                FlapsConf::Conf1F
            }
            (_, 1) => FlapsConf::Conf1,
            (_, 0) => FlapsConf::Conf0,
            (from, to) if from != to => FlapsConf::from(to + 1),
            (_, _) => self.calculated_conf,
        }
    }
}


struct SlatsChannel {
    feedback_angle: Angle,
    demanded_angle: Angle,
    calculated_conf: FlapsConf,

    is_on_ground: bool,

    alpha_lock_engaged: bool,
    alpha_lock_engaged_alpha: bool,
    alpha_lock_engaged_speed: bool,
}
impl SlatsChannel {

    const EQUAL_ANGLE_DELTA_DEGREE: f64 = 0.01;
    const ALPHA_LOCK_ENGAGE_SPEED_KNOTS: f64 = 148.;
    const ALPHA_LOCK_DISENGAGE_SPEED_KNOTS: f64 = 154.;
    const ALPHA_LOCK_ENGAGE_ALPHA_DEGREES: f64 = 8.6;
    const ALPHA_LOCK_DISENGAGE_ALPHA_DEGREES: f64 = 7.6;

    pub fn new() -> Self {
        Self {
            feedback_angle: Angle::new::<degree>(0.),
            demanded_angle: Angle::new::<degree>(0.),
            calculated_conf: FlapsConf::Conf0,
            is_on_ground: true,
            alpha_lock_engaged: false,
            alpha_lock_engaged_alpha: false,
            alpha_lock_engaged_speed: false,
        }
    }

    pub fn update(&mut self, context: &UpdateContext, flaps_handle: &FlapsHandle, feedback: &impl FeedbackPositionPickoffUnit, adiru: &AirDataInertialReferenceUnit, is_on_ground: bool) {

        self.receive_signal(feedback);
        self.is_on_ground = is_on_ground;
        self.alpha_lock_check(context, flaps_handle, adiru);
        self.calculated_conf = self.generate_configuration(context,flaps_handle, adiru);
        self.demanded_angle = demanded_slats_angle_from_conf(self.calculated_conf);

    }

    pub fn update_is_on_ground(&mut self, is_on_ground: bool) {
        self.is_on_ground = is_on_ground;
    }

    fn feedback_equals_demanded(&self) -> bool {
        (self.demanded_angle - self.feedback_angle).get::<degree>().abs() < Self::EQUAL_ANGLE_DELTA_DEGREE
    }

    fn alpha_lock_check(&mut self, context: &UpdateContext, flaps_handle: &FlapsHandle, adiru: &AirDataInertialReferenceUnit) {
        let airspeed: Velocity =
            match adiru.computed_airspeed().ssm() {
                SignStatus::NormalOperation => adiru.computed_airspeed().value(),
                _ => context.indicated_airspeed(),
            };
        let alpha: Angle =  match adiru.alpha().ssm() {
            SignStatus::NormalOperation => adiru.alpha().value(),
            _ => context.alpha(),
        };


        match self.alpha_lock_engaged_speed {
            true => if airspeed.get::<knot>() > Self::ALPHA_LOCK_DISENGAGE_SPEED_KNOTS {
                self.alpha_lock_engaged_speed = false;
            }
            false => if airspeed.get::<knot>() < Self::ALPHA_LOCK_ENGAGE_SPEED_KNOTS {
                self.alpha_lock_engaged_speed = true;
            }
        }

        match self.alpha_lock_engaged_alpha {
            true => if alpha.get::<degree>() < Self::ALPHA_LOCK_DISENGAGE_ALPHA_DEGREES {
                self.alpha_lock_engaged_alpha = false;
            }
            false => if alpha.get::<degree>() > Self::ALPHA_LOCK_ENGAGE_ALPHA_DEGREES {
                self.alpha_lock_engaged_alpha = true;
            }
        }

        match self.alpha_lock_engaged {
            false =>
                if (self.alpha_lock_engaged_alpha || self.alpha_lock_engaged_speed)
                    && flaps_handle.position() > 0
                    && !(self.is_on_ground && airspeed.get::<knot>() < 60.)
                    {
                        self.alpha_lock_engaged = true;
                    }

            true => self.alpha_lock_engaged = self.alpha_lock_engaged_speed || self.alpha_lock_engaged_alpha
        }
    }
}

impl SfccChannel for SlatsChannel {


    fn receive_signal(&mut self, feedback: &impl FeedbackPositionPickoffUnit) {
        self.feedback_angle = feedback.angle();
    }
    fn send_signal(&self) -> bool {
        !self.feedback_equals_demanded()
     }

    fn generate_configuration(
        &self,
        context: &UpdateContext,
        flaps_handle: &FlapsHandle,
        adiru: &AirDataInertialReferenceUnit,
    ) -> FlapsConf {
        match (flaps_handle.previous_position(), flaps_handle.position()) {
            (from, to) if to != 0 => FlapsConf::from(to),
            (from, 0) if self.alpha_lock_engaged && from > 0 => FlapsConf::Conf1,
            (_, 0) => FlapsConf::Conf0,
            (_, _) => self.calculated_conf,
        }
    }
}


struct SlatsFlapsControlComputer {
    flaps_channel: FlapsChannel,
    slats_channel: SlatsChannel,
    is_on_ground: bool,
}

impl SlatsFlapsControlComputer {
    pub fn new() -> Self {
        Self {
            flaps_channel: FlapsChannel::new(),
            slats_channel: SlatsChannel::new(),
            is_on_ground: true,
        }
    }

    pub fn update(&mut self, context: &UpdateContext, flaps_handle: &FlapsHandle,
        adirus: [&AirDataInertialReferenceUnit; 2], lgciu: &LandingGearControlInterfaceUnit,
        flaps_fppu: &impl FeedbackPositionPickoffUnit, slats_fppu: &impl FeedbackPositionPickoffUnit) {

        self.is_on_ground = lgciu.left_and_right_gear_compressed(true);
        self.flaps_channel.update(context, flaps_handle, flaps_fppu, adirus[0]);
        self.slats_channel.update(context, flaps_handle, slats_fppu, adirus[1], self.is_on_ground);
    }
}


#[cfg(test)]
mod test {

    #[test]
    fn dummy_test() {
        assert!(1==1)
    }
}
