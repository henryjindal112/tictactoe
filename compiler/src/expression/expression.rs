//! Methods to enforce constraints on expressions in a compiled Leo program.

use crate::{
    arithmetic::*,
    errors::ExpressionError,
    logical::*,
    program::{new_scope, ConstrainedProgram},
    relational::*,
    value::{boolean::input::new_bool_constant, ConstrainedValue},
    Address,
    FieldType,
    GroupType,
    Integer,
};
use leo_types::{Expression, Identifier, Span, Type};

use snarkos_models::{
    curves::{Field, PrimeField},
    gadgets::r1cs::ConstraintSystem,
};

impl<F: Field + PrimeField, G: GroupType<F>> ConstrainedProgram<F, G> {
    /// Enforce a variable expression by getting the resolved value
    pub(crate) fn evaluate_identifier(
        &mut self,
        file_scope: String,
        function_scope: String,
        expected_types: &Vec<Type>,
        unresolved_identifier: Identifier,
    ) -> Result<ConstrainedValue<F, G>, ExpressionError> {
        // Evaluate the identifier name in the current function scope
        let variable_name = new_scope(function_scope.clone(), unresolved_identifier.to_string());
        let identifier_name = new_scope(file_scope, unresolved_identifier.to_string());

        let mut result_value = if let Some(value) = self.get(&variable_name) {
            // Reassigning variable to another variable
            value.clone()
        } else if let Some(value) = self.get(&identifier_name) {
            // Check global scope (function and circuit names)
            value.clone()
        } else if let Some(value) = self.get(&unresolved_identifier.name) {
            // Check imported file scope
            value.clone()
        } else if expected_types.contains(&Type::Address) {
            // If we expect an address type, try to return an address
            let address = Address::new(unresolved_identifier.name, unresolved_identifier.span)?;

            return Ok(ConstrainedValue::Address(address));
        } else {
            return Err(ExpressionError::undefined_identifier(unresolved_identifier));
        };

        result_value.resolve_type(expected_types, unresolved_identifier.span.clone())?;

        Ok(result_value)
    }

    fn enforce_function_call_expression<CS: ConstraintSystem<F>>(
        &mut self,
        cs: &mut CS,
        file_scope: String,
        function_scope: String,
        expected_types: &Vec<Type>,
        function: Box<Expression>,
        arguments: Vec<Expression>,
        span: Span,
    ) -> Result<ConstrainedValue<F, G>, ExpressionError> {
        let function_value = self.enforce_expression(
            cs,
            file_scope.clone(),
            function_scope.clone(),
            expected_types,
            *function.clone(),
        )?;

        let (outer_scope, function_call) = function_value.extract_function(file_scope.clone(), span.clone())?;

        let name_unique = format!(
            "function call {} {}:{}",
            function_call.get_name(),
            span.line,
            span.start,
        );

        match self.enforce_function(
            &mut cs.ns(|| name_unique),
            outer_scope,
            function_scope,
            function_call,
            arguments,
        ) {
            Ok(ConstrainedValue::Return(return_values)) => {
                if return_values.len() == 1 {
                    Ok(return_values[0].clone())
                } else {
                    Ok(ConstrainedValue::Return(return_values))
                }
            }
            Ok(_) => Err(ExpressionError::function_no_return(function.to_string(), span)),
            Err(error) => Err(ExpressionError::from(Box::new(error))),
        }
    }

    pub(crate) fn enforce_number_implicit(
        expected_types: &Vec<Type>,
        value: String,
        span: Span,
    ) -> Result<ConstrainedValue<F, G>, ExpressionError> {
        if expected_types.len() == 1 {
            return Ok(ConstrainedValue::from_type(value, &expected_types[0], span)?);
        }

        Ok(ConstrainedValue::Unresolved(value))
    }

    /// Enforce a branch of a binary expression.
    /// We don't care about mutability because we are not changing any variables.
    /// We try to resolve unresolved types here if the type is given explicitly.
    pub(crate) fn enforce_expression_value<CS: ConstraintSystem<F>>(
        &mut self,
        cs: &mut CS,
        file_scope: String,
        function_scope: String,
        expected_types: &Vec<Type>,
        expression: Expression,
        span: Span,
    ) -> Result<ConstrainedValue<F, G>, ExpressionError> {
        let mut branch = self.enforce_expression(cs, file_scope, function_scope, expected_types, expression)?;

        branch.get_inner_mut();
        branch.resolve_type(expected_types, span)?;

        Ok(branch)
    }

    pub(crate) fn enforce_binary_expression<CS: ConstraintSystem<F>>(
        &mut self,
        cs: &mut CS,
        file_scope: String,
        function_scope: String,
        expected_types: &Vec<Type>,
        left: Expression,
        right: Expression,
        span: Span,
    ) -> Result<(ConstrainedValue<F, G>, ConstrainedValue<F, G>), ExpressionError> {
        let mut resolved_left = self.enforce_expression_value(
            cs,
            file_scope.clone(),
            function_scope.clone(),
            expected_types,
            left,
            span.clone(),
        )?;
        let mut resolved_right = self.enforce_expression_value(
            cs,
            file_scope.clone(),
            function_scope.clone(),
            expected_types,
            right,
            span.clone(),
        )?;

        resolved_left.resolve_types(&mut resolved_right, expected_types, span)?;

        Ok((resolved_left, resolved_right))
    }

    pub(crate) fn enforce_expression<CS: ConstraintSystem<F>>(
        &mut self,
        cs: &mut CS,
        file_scope: String,
        function_scope: String,
        expected_types: &Vec<Type>,
        expression: Expression,
    ) -> Result<ConstrainedValue<F, G>, ExpressionError> {
        match expression {
            // Variables
            Expression::Identifier(unresolved_variable) => {
                self.evaluate_identifier(file_scope, function_scope, expected_types, unresolved_variable)
            }

            // Values
            Expression::Address(address, span) => Ok(ConstrainedValue::Address(Address::new(address, span)?)),
            Expression::Boolean(boolean, span) => Ok(ConstrainedValue::Boolean(new_bool_constant(boolean, span)?)),
            Expression::Field(field, span) => Ok(ConstrainedValue::Field(FieldType::constant(field, span)?)),
            Expression::Group(group_affine, span) => Ok(ConstrainedValue::Group(G::constant(group_affine, span)?)),
            Expression::Implicit(value, span) => Self::enforce_number_implicit(expected_types, value, span),
            Expression::Integer(type_, integer, span) => {
                Ok(ConstrainedValue::Integer(Integer::new_constant(&type_, integer, span)?))
            }

            // Binary operations
            Expression::Add(left, right, span) => {
                let (resolved_left, resolved_right) = self.enforce_binary_expression(
                    cs,
                    file_scope.clone(),
                    function_scope.clone(),
                    expected_types,
                    *left,
                    *right,
                    span.clone(),
                )?;

                enforce_add_expression(cs, resolved_left, resolved_right, span)
            }
            Expression::Sub(left, right, span) => {
                let (resolved_left, resolved_right) = self.enforce_binary_expression(
                    cs,
                    file_scope.clone(),
                    function_scope.clone(),
                    expected_types,
                    *left,
                    *right,
                    span.clone(),
                )?;

                enforce_sub_expression(cs, resolved_left, resolved_right, span)
            }
            Expression::Mul(left, right, span) => {
                let (resolved_left, resolved_right) = self.enforce_binary_expression(
                    cs,
                    file_scope.clone(),
                    function_scope.clone(),
                    expected_types,
                    *left,
                    *right,
                    span.clone(),
                )?;

                enforce_mul_expression(cs, resolved_left, resolved_right, span)
            }
            Expression::Div(left, right, span) => {
                let (resolved_left, resolved_right) = self.enforce_binary_expression(
                    cs,
                    file_scope.clone(),
                    function_scope.clone(),
                    expected_types,
                    *left,
                    *right,
                    span.clone(),
                )?;

                enforce_div_expression(cs, resolved_left, resolved_right, span)
            }
            Expression::Pow(left, right, span) => {
                let (resolved_left, resolved_right) = self.enforce_binary_expression(
                    cs,
                    file_scope.clone(),
                    function_scope.clone(),
                    expected_types,
                    *left,
                    *right,
                    span.clone(),
                )?;

                enforce_pow_expression(cs, resolved_left, resolved_right, span)
            }

            // Boolean operations
            Expression::Not(expression, span) => Ok(evaluate_not(
                self.enforce_expression(cs, file_scope, function_scope, expected_types, *expression)?,
                span,
            )?),
            Expression::Or(left, right, span) => {
                let (resolved_left, resolved_right) = self.enforce_binary_expression(
                    cs,
                    file_scope.clone(),
                    function_scope.clone(),
                    expected_types,
                    *left,
                    *right,
                    span.clone(),
                )?;

                Ok(enforce_or(cs, resolved_left, resolved_right, span)?)
            }
            Expression::And(left, right, span) => {
                let (resolved_left, resolved_right) = self.enforce_binary_expression(
                    cs,
                    file_scope.clone(),
                    function_scope.clone(),
                    expected_types,
                    *left,
                    *right,
                    span.clone(),
                )?;

                Ok(enforce_and(cs, resolved_left, resolved_right, span)?)
            }
            Expression::Eq(left, right, span) => {
                let (resolved_left, resolved_right) = self.enforce_binary_expression(
                    cs,
                    file_scope.clone(),
                    function_scope.clone(),
                    &vec![],
                    *left,
                    *right,
                    span.clone(),
                )?;

                Ok(evaluate_eq_expression(cs, resolved_left, resolved_right, span)?)
            }
            Expression::Ge(left, right, span) => {
                let (resolved_left, resolved_right) = self.enforce_binary_expression(
                    cs,
                    file_scope.clone(),
                    function_scope.clone(),
                    &vec![],
                    *left,
                    *right,
                    span.clone(),
                )?;

                Ok(evaluate_ge_expression(cs, resolved_left, resolved_right, span)?)
            }
            Expression::Gt(left, right, span) => {
                let (resolved_left, resolved_right) = self.enforce_binary_expression(
                    cs,
                    file_scope.clone(),
                    function_scope.clone(),
                    &vec![],
                    *left,
                    *right,
                    span.clone(),
                )?;

                Ok(evaluate_gt_expression(cs, resolved_left, resolved_right, span)?)
            }
            Expression::Le(left, right, span) => {
                let (resolved_left, resolved_right) = self.enforce_binary_expression(
                    cs,
                    file_scope.clone(),
                    function_scope.clone(),
                    &vec![],
                    *left,
                    *right,
                    span.clone(),
                )?;

                Ok(evaluate_le_expression(cs, resolved_left, resolved_right, span)?)
            }
            Expression::Lt(left, right, span) => {
                let (resolved_left, resolved_right) = self.enforce_binary_expression(
                    cs,
                    file_scope.clone(),
                    function_scope.clone(),
                    &vec![],
                    *left,
                    *right,
                    span.clone(),
                )?;

                Ok(evaluate_lt_expression(cs, resolved_left, resolved_right, span)?)
            }

            // Conditionals
            Expression::IfElse(conditional, first, second, span) => self.enforce_conditional_expression(
                cs,
                file_scope,
                function_scope,
                expected_types,
                *conditional,
                *first,
                *second,
                span,
            ),

            // Arrays
            Expression::Array(array, span) => {
                self.enforce_array_expression(cs, file_scope, function_scope, expected_types, array, span)
            }
            Expression::ArrayAccess(array, index, span) => self.enforce_array_access_expression(
                cs,
                file_scope,
                function_scope,
                expected_types,
                array,
                *index,
                span,
            ),

            // Circuits
            Expression::Circuit(circuit_name, members, span) => {
                self.enforce_circuit_expression(cs, file_scope, function_scope, circuit_name, members, span)
            }
            Expression::CircuitMemberAccess(circuit_variable, circuit_member, span) => self
                .enforce_circuit_access_expression(
                    cs,
                    file_scope,
                    function_scope,
                    expected_types,
                    circuit_variable,
                    circuit_member,
                    span,
                ),
            Expression::CircuitStaticFunctionAccess(circuit_identifier, circuit_member, span) => self
                .enforce_circuit_static_access_expression(
                    cs,
                    file_scope,
                    function_scope,
                    expected_types,
                    circuit_identifier,
                    circuit_member,
                    span,
                ),

            // Functions
            Expression::FunctionCall(function, arguments, span) => self.enforce_function_call_expression(
                cs,
                file_scope,
                function_scope,
                expected_types,
                function,
                arguments,
                span,
            ),
        }
    }
}
